from __future__ import annotations

import contextlib
import errno
import fcntl
import hashlib
import io
import json
import os
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import types
import pathlib
import pytest
import runpy
import unittest
from unittest import mock
from collections.abc import Callable
from pathlib import Path


# --import-mode=importlib does not put this directory on sys.path. Insert it
# here so the sibling test-module imports (at module level and inside tests) do
# not depend on another module having inserted it earlier in the same session.
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import start_unit  # noqa: E402
import run_registry  # noqa: E402
from test_immutable_tool_authority import AuthorityFixture, make_tree_read_only
import qualifying_receipt  # noqa: E402
from scripts import test_parent_main_write as parent_main_tests  # noqa: E402


SHA = "a" * 40
HERMIT_SHA = "b" * 40
AGENT_UTILS_SHA = "c" * 40
WRKSLOTS_ACTIONS = (
    "classify-create-journals",
    "classify-ownerless-validate-batch",
    "create",
    "recover",
    "recover-ownerless-validate-batch",
    "remove",
    "remove-validate-batch",
    "status",
)
PRE_POPULATION_WRITER_COMMIT = "4037baed51b52942b570c66718f8d0bfccf63a03"
PRODUCTION_CLEANUP_ROLLOUT_ENABLED = (
    start_unit.DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED
)


@pytest.fixture(autouse=True)
def exercise_post_quiescence_cleanup(monkeypatch: pytest.MonkeyPatch) -> None:
    """Exercise deletion mechanics without changing the production source fence.

    External probes that merely import this module receive the real fail-closed
    setting; only pytest-managed test cases opt into the hypothetical second
    rollout stage.  Dedicated fence tests override this fixture back to false.
    """

    monkeypatch.setattr(start_unit, "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED", True)


def completed(command: list[str], rc: int = 0, stdout: str = "", stderr: str = ""):
    return subprocess.CompletedProcess(command, rc, stdout, stderr)


def wrkslots_action(command: list[str], action: str) -> bool:
    return command[0].endswith("ci-hub/bin/wrkslots") and action in command


def assert_local_wrkslots_machine(command: list[str]) -> None:
    if not command[0].endswith("ci-hub/bin/wrkslots"):
        return
    assert command.count("--machine") == 1, command
    machine_index = command.index("--machine")
    assert command[machine_index + 1] == start_unit.WRKSLOTS_MACHINE, command
    actions = [action for action in WRKSLOTS_ACTIONS if action in command]
    assert len(actions) == 1, command
    assert machine_index < command.index(actions[0]), command


def publication_report(target: str) -> dict:
    return {
        "state": "published", "commit": "d" * 40,
        "source_commit": "e" * 40, "source_tree": "f" * 40, "rows": 17,
        "repository": start_unit.CELL_LEDGER_REPOSITORY,
        "source_repository": start_unit.CELL_LEDGER_REPOSITORY,
        "series_commit": "e" * 40, "raw_publication": {"published": 17},
        "scorecard": {
            "state": "published", "hermit_commit": target,
            "files": {"scorecard/SCORECARD.md": "1" * 64, "scorecard/cells.json": "2" * 64},
        },
    }


class FakeRun:
    def __init__(self, checkout: Path) -> None:
        self.checkout = checkout
        self.target = SHA
        self.commands: list[list[str]] = []
        self.command_cwds: list[tuple[list[str], Path | None]] = []
        self.command_envs: list[dict[str, str] | None] = []
        self.dirty = ""
        self.fresh_dirty = ""
        self.admission_rc = 0
        self.typed_admission_refusal: str | None = None
        self.herdr_status_rc = 0
        # Fresh-checkout controls: `fresh` is what `mktemp -d` hands back,
        # `fresh_complete` decides whether the tree carries what validate needs,
        # and `ledger_rows` is what the canonical reader returns.
        self.fresh: Path | None = None
        # (uncommitted paths, commits on no remote ref) the gate should see.
        self.fresh_authored_work: tuple[int, int] = (0, 0)
        self.fresh_complete = True
        self.fresh_runner_name = "safe-ci-dag-runner"
        self.fresh_runner_executable = True
        self.fresh_head = self.target
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
        self.passed_tests: int | None = 862
        self.detail: list[str] | None = None
        self.service_result_schema = start_unit.service_result.SCHEMA_VERSION
        self.include_passed_tests = True
        self.write_service_result = True
        self.write_frozen_result = True
        self.collected_unit = False
        self.current_main = "b" * 40
        self.target_contains_current_main = False
        self.source_common_dir_available = True
        self.source_head = self.target
        self.source_head_rc = 0
        self.source_target = self.target
        self.source_target_rc = 0
        self.source_branch: str | None = "codex/fixture-branch"
        self.source_branch_rc = 0
        self.real_source_git = False
        self.scorecard_update = False
        self.scorecard_rc = 0
        self.scorecard_stderr = ""
        self.scorecard_head_after_write: str | None = None
        self.ledger_mirror_rc = 0
        self.ledger_mirror_stderr = ""
        self.ledger_mirror_update: dict = {}
        self.commit_status_action = "published"
        self.commit_status_rc = 0
        self.commit_status_stderr = ""
        self.commit_status_launch_error: OSError | None = None
        self.ownerless_cargo_recovery_rc = 0
        self.wrkslots_generation = 7
        self.wrkslots_create_output: str | None = None
        self.create_journal_classification_output: str | None = None
        self.wrkslots_create_path: str | None = None
        self.wrkslots_status_generation: int | None = None
        self.wrkslots_status_path: str | None = None
        self.wrkslots_status_head: str | None = None
        self.wrkslots_status_rc = 0
        self.wrkslots_status_stderr = ""
        self.wrkslots_status_output: str | None = None
        self.wrkslots_status_active: bool | None = None
        self.wrkslots_remove_rc = 0
        self.wrkslots_remove_stderr = ""
        self.wrkslots_remove_keeps_checkout_on_error = True
        self.wrkslots_remove_keeps_row_on_error = True
        self.registered_validate_slots: set[tuple[str, int]] = set()
        self.scorecard_tamper_on_status: str | None = None
        self.validate_batch_retained: dict[str, str] = {}
        self.validate_batch_output: str | None = None
        self.validate_batch_zero_census = False
        self.ownerless_batch_retained: dict[str, str] = {}
        self.ownerless_batch_nonblocking: set[str] = set()
        self.ownerless_batch_output: str | None = None
        self.systemd_launch_rc = 0
        self.systemd_launch_stderr = ""
        self.systemd_checkout_head: str | None = None
        self.inner_log_lines: list[str] = []
        # Successive `git submodule status` outputs for the fresh checkout; the
        # last entry repeats. The default is one registered, current gitlink.
        # There are two views. The RUN view is a status read through the run's
        # captured-registration override (an includeIf.gitdir GIT_CONFIG_*
        # entry in the child environment): the creation gate and the run's own
        # Git processes. The SHARED view is a status read without it: what
        # every other worktree sees. None means the shared view mirrors the run
        # view's outputs.
        self.fresh_submodule_status: list[str] = [f" {SHA} agent-utils (heads/main)\n"]
        self.fresh_submodule_status_calls = 0
        self.fresh_shared_submodule_status: list[str] | None = None
        self.fresh_shared_submodule_status_calls = 0
        self.fresh_submodule_status_views: list[str] = []
        # Successive `git config --local --null --get-regexp ^submodule\.`
        # outputs (the shared registration); the last entry repeats.
        self.fresh_submodule_config: list[str] = [
            "submodule.agent-utils.url\nhttps://github.com/rrnewton/agent-utils.git\0"
            "submodule.agent-utils.active\ntrue\0"
            "submodule.agent-utils.update\ncheckout\0"
        ]
        self.fresh_submodule_config_calls = 0
        self.fresh_common_dir: Path | None = None


    def fresh_git_dir(self) -> Path:
        """The fresh checkout's own Git directory, as a linked worktree has it."""
        assert self.fresh is not None
        git_dir = self.checkout / ".git" / "worktrees" / self.fresh.name
        git_dir.mkdir(parents=True, exist_ok=True)
        return git_dir

    def default_ledger_row(self) -> dict[str, object]:
        return {
            "commit": self.target,
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

    def __call__(self, command: list[str], **kwargs: object):
        self.commands.append(command)
        assert_local_wrkslots_machine(command)
        # THE REMOVAL PATH NOW ASKS THE TREE ABOUT ITSELF, so the fake must be
        # able to answer. These fixtures deliberately model a checkout as a
        # directory rather than paying for a real linked worktree, and the two
        # questions the authorisation gate asks -- uncommitted paths, and
        # commits reaching no remote ref -- are exactly the kind of thing this
        # runner already stands in for. Answering them here keeps the fixtures
        # cheap AND lets the gate be exercised; the alternative, making every
        # fixture a real `git worktree add`, was tried and is a different and
        # much larger change. `fresh_authored_work` lets a test say the tree is
        # dirty or carries an off-remote commit, so the refusal paths are
        # reachable too.
        if len(command) >= 4 and command[0] == "git" and command[1] == "-C":
            dirty, unpushed = self.fresh_authored_work
            if command[3:] == ["status", "--porcelain"]:
                return completed(command, stdout="\n".join("?? f" for _ in range(dirty)))
            if command[3:] == ["rev-list", "HEAD", "--not", "--remotes"]:
                return completed(command, stdout="\n".join("0" * 40 for _ in range(unpushed)))
        cwd = kwargs.get("cwd")
        child_environment = kwargs.get("env")
        self.last_child_environment = child_environment
        self.command_cwds.append(
            (command, cwd if isinstance(cwd, Path) else Path(cwd) if cwd else None)
        )
        environment = kwargs.get("env")
        self.command_envs.append(dict(environment) if isinstance(environment, dict) else None)
        if (
            command[0].endswith("ci-hub/bin/wrkslots")
            and "classify-create-journals" in command
        ):
            if self.create_journal_classification_output is not None:
                return completed(command, stdout=self.create_journal_classification_output)
            project_root = Path(command[command.index("--project-root") + 1])
            slot = command[command.index("classify-create-journals") + 1]
            slot_type = command[command.index("--slot-type") + 1]
            agent = command[command.index("--agent") + 1]
            return completed(
                command,
                stdout=json.dumps(
                    {
                        "blocking": False,
                        "journals": [],
                        "requested": {
                            "agent": agent,
                            "slot": slot,
                            "slot_path": str(
                                project_root / "worktrees" / "validate" / slot
                            ),
                            "slot_type": slot_type,
                        },
                        "schema": 2,
                    }
                ),
            )
        if command[0].endswith("ci-hub/bin/wrkslots") and "create" in command:
            slot = command[command.index("create") + 1]
            project_root = Path(command[command.index("--project-root") + 1])
            self.registered_validate_slots.add((slot, self.wrkslots_generation))
            self.wrkslots_status_active = True
            self.fresh = project_root / "worktrees" / "validate" / slot
            if self.real_source_git:
                self.fresh.parent.mkdir(parents=True, exist_ok=True)
                subprocess.run(
                    [
                        "git",
                        "-C",
                        str(self.checkout),
                        "worktree",
                        "add",
                        "--detach",
                        str(self.fresh),
                        self.target,
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                    env=(
                        dict(child_environment)
                        if isinstance(child_environment, dict)
                        else None
                    ),
                )
            else:
                self._materialize_fresh_checkout()
            if self.wrkslots_create_output is not None:
                return completed(command, stdout=self.wrkslots_create_output)
            return completed(
                command,
                stdout=json.dumps(
                    {
                        "agent": f"validate-{slot}",
                        "checkouts": [
                            {
                                "head": self.target,
                                "name": "checkout",
                                "path": self.wrkslots_create_path
                                or str(self.fresh.resolve()),
                            }
                        ],
                        "generation": self.wrkslots_generation,
                        "owner_process": "bound",
                        "retained_storage_inconsistencies": [],
                        "slot": slot,
                        "slot_type": "validate",
                    }
                ),
            )
        if command[0].endswith("ci-hub/bin/wrkslots") and "status" in command:
            if self.wrkslots_status_rc != 0:
                return completed(
                    command,
                    rc=self.wrkslots_status_rc,
                    stderr=self.wrkslots_status_stderr,
                )
            if self.wrkslots_status_output is not None:
                return completed(command, stdout=self.wrkslots_status_output)
            project_root = Path(command[command.index("--project-root") + 1])
            slots = (
                [command[command.index("--slot") + 1]]
                if "--slot" in command
                else sorted(
                    {
                        *(slot for slot, _generation in self.registered_validate_slots),
                        *(
                            path.name
                            for path in (project_root / "worktrees/validate").glob(
                                "validate-fresh-*"
                            )
                            if path.is_dir()
                        ),
                    }
                )
            )
            status_checkout = (
                project_root / "worktrees/validate" / slots[0] if slots else None
            )
            if self.scorecard_tamper_on_status is not None and status_checkout is not None:
                tampered = status_checkout / self.scorecard_tamper_on_status
                tampered.write_text(tampered.read_text() + "tampered after writeback\n")
                self.fresh_authored_work = (1, 0)
            active = (
                []
                if self.wrkslots_status_active is False
                else [
                    {
                        "slot": slot,
                        "slot_type": "validate",
                        "generation": (
                            self.wrkslots_generation
                            if self.wrkslots_status_generation is None
                            else self.wrkslots_status_generation
                        ),
                        "storage_inconsistencies": [],
                        "checkouts": [
                            {
                                "name": "checkout",
                                "path": (
                                    self.wrkslots_status_path
                                    or (
                                        project_root / "worktrees/validate" / slot
                                    ).relative_to(project_root).as_posix()
                                ),
                                "head": self.wrkslots_status_head or self.target,
                            }
                        ],
                    }
                    for slot in slots
                ]
            )
            return completed(
                command,
                stdout=json.dumps(
                    {
                        "schema": 2,
                        "project_root": str(project_root.resolve()),
                        "active": active,
                    }
                ),
            )
        if (
            command[0].endswith("ci-hub/bin/wrkslots")
            and any(
                action in command
                for action in (
                    "classify-ownerless-validate-batch",
                    "recover-ownerless-validate-batch",
                )
            )
        ):
            if self.ownerless_batch_output is not None:
                return completed(command, stdout=self.ownerless_batch_output)
            classify_only = "classify-ownerless-validate-batch" in command
            if not classify_only and "--coordinator-authorized" not in command:
                raise AssertionError(
                    "ownerless validation batch cleanup must carry explicit authority"
                )
            if classify_only and any(
                value in command
                for value in ("--coordinator-authorized", "--coordinator-pid")
            ):
                raise AssertionError(
                    "ownerless validation classification must not carry mutation authority"
                )
            project_root = Path(command[command.index("--project-root") + 1])
            checkout_flag = (
                "--frozen-validate-checkout"
                if "--frozen-validate-checkout" in command
                else "--checkout"
            )
            relative_checkouts = [
                command[index + 1]
                for index, value in enumerate(command[:-1])
                if value == checkout_flag
            ]
            records = [
                command[index + 1]
                for index, value in enumerate(command[:-1])
                if value == "--completed-record"
            ]
            repositories = [
                command[index + 1]
                for index, value in enumerate(command[:-1])
                if value == "--repository"
            ]
            if not len(relative_checkouts) == len(records) == len(repositories):
                raise AssertionError("ownerless validation batch arguments are not aligned")
            removed_rows: list[dict[str, object]] = []
            retained_rows: list[dict[str, object]] = []
            for relative in relative_checkouts:
                reason = self.ownerless_batch_retained.get(relative)
                if reason is not None:
                    retained_rows.append(
                        {
                            "blocks_entry": relative
                            not in self.ownerless_batch_nonblocking,
                            "checkout": relative,
                            "reason": reason,
                        }
                    )
                    continue
                fresh = project_root / relative
                if classify_only:
                    retained_rows.append(
                        {
                            "blocks_entry": False,
                            "checkout": relative,
                            "reason": (
                                "terminal validation is exact and unused; read-only "
                                "classification left it retained"
                            ),
                        }
                    )
                else:
                    self.removed.append(str(fresh))
                    shutil.rmtree(fresh, ignore_errors=True)
                    removed_rows.append({"checkout": relative})
            if classify_only:
                return completed(
                    command,
                    stdout=json.dumps(
                        {
                            "schema": 1,
                            "process_censuses": 1,
                            "requested": len(relative_checkouts),
                            "create_journals": [],
                            "classifications": [
                                {
                                    **row,
                                    "state": (
                                        "could-not-classify"
                                        if row["blocks_entry"]
                                        else "historical-retained"
                                        if str(row["reason"]).startswith(
                                            "historical schema "
                                        )
                                        else "terminal-retained"
                                    ),
                                }
                                for row in retained_rows
                            ],
                        }
                    ),
                )
            return completed(
                command,
                stdout=json.dumps(
                    {
                        "schema": 2,
                        "batch_limit": start_unit.VALIDATE_CLEANUP_BATCH_LIMIT,
                        "process_censuses": 1,
                        "shared_process_censuses": 1,
                        "same_uid_process_censuses": len(removed_rows)
                        + sum(
                            row["blocks_entry"] is False
                            for row in retained_rows
                        ),
                        "requested": len(relative_checkouts),
                        "removed": removed_rows,
                        "retained": retained_rows,
                    }
                ),
            )
        if (
            command[0].endswith("ci-hub/bin/wrkslots")
            and "remove-validate-batch" in command
        ):
            if self.validate_batch_output is not None:
                return completed(command, stdout=self.validate_batch_output)
            project_root = Path(command[command.index("--project-root") + 1])
            identities = [
                value.rsplit("=", 1)
                for index, value in enumerate(command)
                if index > 0 and command[index - 1] == "--slot"
            ]
            removed_rows: list[dict[str, object]] = []
            retained_rows: list[dict[str, object]] = []
            for slot, raw_generation in identities:
                generation = int(raw_generation)
                reason = self.validate_batch_retained.get(slot)
                if reason is not None:
                    retained_rows.append(
                        {"slot": slot, "generation": generation, "reason": reason}
                    )
                    continue
                fresh = project_root / "worktrees" / "validate" / slot
                self.removed.append(str(fresh))
                shutil.rmtree(fresh, ignore_errors=True)
                removed_rows.append({"slot": slot, "generation": generation})
            shared_censuses = 0 if self.validate_batch_zero_census else 1
            return completed(
                command,
                stdout=json.dumps(
                    {
                        "schema": 1,
                        "batch_limit": start_unit.VALIDATE_CLEANUP_BATCH_LIMIT,
                        "process_censuses": shared_censuses,
                        "shared_process_censuses": shared_censuses,
                        "same_uid_process_censuses": (
                            0 if shared_censuses == 0 else len(removed_rows)
                        ),
                        "requested": len(identities),
                        "removed": removed_rows,
                        "retained": retained_rows,
                    }
                ),
            )
        if command[0].endswith("ci-hub/bin/wrkslots") and "remove" in command:
            if self.wrkslots_remove_rc != 0:
                if self.fresh is not None:
                    slot = command[command.index("remove") + 1]
                    generation = int(
                        command[command.index("--expected-generation") + 1]
                    )
                    if not self.wrkslots_remove_keeps_checkout_on_error:
                        shutil.rmtree(self.fresh, ignore_errors=True)
                    if not self.wrkslots_remove_keeps_row_on_error:
                        self.registered_validate_slots.discard((slot, generation))
                self.wrkslots_status_active = self.wrkslots_remove_keeps_row_on_error
                return completed(
                    command,
                    rc=self.wrkslots_remove_rc,
                    stderr=self.wrkslots_remove_stderr,
                )
            if self.fresh is not None:
                slot = command[command.index("remove") + 1]
                generation = int(command[command.index("--expected-generation") + 1])
                self.registered_validate_slots.discard((slot, generation))
                self.wrkslots_status_active = False
                self.removed.append(str(self.fresh))
                if self.real_source_git:
                    subprocess.run(
                        [
                            "git",
                            "-C",
                            str(self.checkout),
                            "worktree",
                            "remove",
                            "--force",
                            str(self.fresh),
                        ],
                        check=True,
                        capture_output=True,
                        text=True,
                        env=(
                            dict(child_environment)
                            if isinstance(child_environment, dict)
                            else None
                        ),
                    )
                else:
                    shutil.rmtree(self.fresh, ignore_errors=True)
            return completed(command, stdout="removed and archived validation slot\n")
        if (
            command[0].endswith("ci-hub/bin/wrkslots")
            and "recover" in command
            and "--legacy-validate-checkout" in command
        ):
            if "--coordinator-authorized" not in command:
                raise AssertionError(
                    "legacy validation cleanup must carry explicit coordinator authorization"
                )
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
        if (
            command[0].endswith("ci-hub/bin/wrkslots")
            and "recover" in command
            and "--ownerless-validate-cargo-home" in command
        ):
            required = (
                "--coordinator-authorized",
                "--coordinator-pid",
                "--completed-record",
            )
            missing = [flag for flag in required if flag not in command]
            if missing:
                raise AssertionError(
                    "recorded Cargo-home recovery omitted " + ", ".join(missing)
                )
            project_root = Path(command[command.index("--project-root") + 1])
            relative = Path(
                command[command.index("--ownerless-validate-cargo-home") + 1]
            )
            completed_record = Path(
                command[command.index("--completed-record") + 1]
            )
            if relative.is_absolute() or completed_record.is_absolute():
                raise AssertionError("wrkslots recovery paths must be project-relative")
            if self.ownerless_cargo_recovery_rc != 0:
                return completed(
                    command,
                    rc=self.ownerless_cargo_recovery_rc,
                    stderr="ownerless validation recovery refused",
                )
            private = project_root / relative
            self.removed.append(str(private))
            shutil.rmtree(private, ignore_errors=True)
            return completed(
                command,
                stdout="recovered ownerless validation cargo-home\n",
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
        (self.fresh / ".git").write_text("gitdir: shared/worktrees/fake\n")
        (self.fresh / "scripts").mkdir(parents=True, exist_ok=True)
        (self.fresh / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
        compat = self.fresh / "ci/compat-envelope"
        compat.mkdir(parents=True, exist_ok=True)
        (compat / "scorecard.rs").write_text("#!/usr/bin/env rust-script\n")
        (self.fresh / "SCORECARD.md").write_text("scorecard\n")
        (compat / "cells.json").write_text("{}\n")
        source_schema = (
            self.checkout / "ci/manifest-plan/validation-service-result-schema.json"
        )
        if source_schema.exists():
            target_schema = (
                self.fresh / "ci/manifest-plan/validation-service-result-schema.json"
            )
            target_schema.parent.mkdir(parents=True, exist_ok=True)
            target_schema.write_text(source_schema.read_text())
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
            receipt.write_text(
                f'{{"commit": "{self.target}", "cwd": "{self.fresh}"}}\n'
            )

    def _run_after_checkout_setup(self, command: list[str]):
        environment = getattr(self, "last_child_environment", None)
        if command[:2] == ["rm", "-rf"]:
            shutil.rmtree(command[-1], ignore_errors=True)
            return completed(command)
        if command[:1] == ["timeout"] and "fetch" in command:
            return completed(command)
        if command[:2] == ["git", "clone"]:
            self.fresh = Path(command[-1])
            self._materialize_fresh_checkout()
            return completed(command)
        if self.real_source_git and command[:3] in (
            ["git", "-C", str(self.checkout)],
            ["git", "-C", str(self.fresh)],
        ):
            return subprocess.run(
                command,
                check=False,
                capture_output=True,
                text=True,
                env=(
                    dict(environment)
                    if isinstance(environment, dict)
                    else None
                ),
            )
        if (
            command[:1] == ["git"]
            and "-C" in command
            and Path(command[command.index("-C") + 1]).parent
            == self.checkout.parent / "worktrees/validate"
            and command[-2:] == ["rev-parse", "HEAD^{commit}"]
        ):
            location = Path(command[command.index("-C") + 1])
            head = self.fresh_head if location == self.fresh else self.target
            return completed(command, stdout=f"{head}\n")
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
                common = self.fresh_common_dir or self.checkout / ".git"
                common.mkdir(parents=True, exist_ok=True)
                return completed(command, stdout=f"{common}\n")
            if command[-1] == "--absolute-git-dir":
                return completed(command, stdout=f"{self.fresh_git_dir()}\n")
            return completed(command, stdout=f"{self.fresh_head}\n")
        if (
            self.fresh is not None
            and command[:4] == ["git", "-C", str(self.fresh), "status"]
        ):
            return completed(command, stdout=self.fresh_dirty)
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
        if self.fresh is not None and command[:5] == ["git", "-C", str(self.fresh), "submodule", "status"]:
            run_view = isinstance(environment, dict) and any(
                key.startswith("GIT_CONFIG_KEY_")
                and value
                == "includeIf.gitdir:"
                + f"{self.fresh_git_dir().resolve()}.path"
                and environment.get("GIT_CONFIG_VALUE_" + key.removeprefix("GIT_CONFIG_KEY_"))
                == str(self.fresh_git_dir().resolve() / "validate-submodule-registration")
                for key, value in environment.items()
            )
            self.fresh_submodule_status_views.append("run" if run_view else "shared")
            if run_view or self.fresh_shared_submodule_status is None:
                statuses = self.fresh_submodule_status
                index = min(self.fresh_submodule_status_calls, len(statuses) - 1)
                self.fresh_submodule_status_calls += 1
            else:
                statuses = self.fresh_shared_submodule_status
                index = min(self.fresh_shared_submodule_status_calls, len(statuses) - 1)
                self.fresh_shared_submodule_status_calls += 1
            return completed(command, stdout=statuses[index])
        if (
            self.fresh is not None
            and command[:4] == ["git", "-C", str(self.fresh), "config"]
            and command[-3:] == ["--null", "--get-regexp", r"^submodule\."]
        ):
            if command[4:-3] == ["--local"]:
                index = min(
                    self.fresh_submodule_config_calls, len(self.fresh_submodule_config) - 1
                )
                self.fresh_submodule_config_calls += 1
                stdout = self.fresh_submodule_config[index]
            else:
                # A captured file is read back with `--file`; the real one says
                # what the launcher wrote there.
                assert command[4] == "--file" and len(command) == 9, command
                read = subprocess.run(
                    ["git", *command[3:]], capture_output=True, text=True, check=False
                )
                assert read.returncode in (0, 1), read.stderr
                stdout = read.stdout
            return completed(command, stdout=stdout, rc=0 if stdout else 1)
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
        if len(command) > 1 and command[0].endswith("ci-hub") and command[1] == "publish-commit-status":
            if self.commit_status_launch_error is not None:
                raise self.commit_status_launch_error
            report = {
                "schema_version": 1,
                "action": self.commit_status_action,
                "repository": command[command.index("--repo") + 1],
                "sha": command[command.index("--sha") + 1],
                "context": "Local validation",
                "description": "Local full: 1 selected cell; 1/1 test nodes; 1/1 outer gates",
                "receipt_commit": "c" * 40,
                "receipt_path": "validation-receipts/fixture.json",
            }
            return completed(
                command,
                rc=self.commit_status_rc,
                stdout=json.dumps(report) if self.commit_status_rc == 0 else "",
                stderr=self.commit_status_stderr,
            )
        if command[0].endswith("ci-hub") and command[1:3] == ["validate-status", "--sha"]:
            verdict = self.canonical_verdict
            status_rc = self.canonical_status_rc
            rows = self.ledger_rows
            if rows is None:
                rows = [json.dumps(self.default_ledger_row())]
            qualifying = len(rows) if verdict == "VALIDATED" else 0
            parsed_rows = [json.loads(row) for row in rows]
            report = {
                "sha": self.target,
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
        if len(command) >= 4 and command[-3].endswith("validate_rows.py") and command[-2:] == ["rows", "--preserve-admission"]:
            rows = self.ledger_rows
            if rows is None:
                rows = [json.dumps(self.default_ledger_row())]
            return completed(command, stdout="\n".join(rows) + ("\n" if rows else ""))
        if (
            command[:4] == ["git", "-C", str(self.checkout), "rev-parse"]
            and command[-1] == "refs/remotes/origin/main^{commit}"
        ):
            return completed(command, stdout=f"{self.current_main}\n")
        if command[:4] == ["git", "-C", str(self.checkout), "symbolic-ref"]:
            if self.source_branch_rc != 0:
                return completed(
                    command, rc=self.source_branch_rc, stderr="source branch unreadable\n"
                )
            if self.source_branch is None:
                return completed(command, rc=1)
            return completed(command, stdout=f"{self.source_branch}\n")
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
            if command[-1] == f"{self.target}^{{commit}}":
                return completed(
                    command,
                    rc=self.source_target_rc,
                    stdout=f"{self.source_target}\n" if self.source_target_rc == 0 else "",
                    stderr=(
                        "fatal: bad object\n" if self.source_target_rc != 0 else ""
                    ),
                )
            if command[-1] == "HEAD^{commit}" and self.source_head_rc != 0:
                return completed(
                    command, rc=self.source_head_rc, stderr="source HEAD unreadable\n"
                )
            return completed(command, stdout=f"{self.source_head}\n")
        if command[:4] == ["git", "-C", str(self.checkout), "status"]:
            return completed(command, stdout=self.dirty)
        if command[0].endswith("ci/compat-envelope/scorecard.rs"):
            scorecard_checkout = Path(command[0]).parents[2]
            if self.scorecard_rc == 0 and self.scorecard_update:
                for relative in start_unit.SCORECARD_PATHS:
                    path = scorecard_checkout / relative
                    path.write_text(path.read_text() + "updated\n")
            if self.scorecard_rc == 0 and self.scorecard_head_after_write is not None:
                self.fresh_head = self.scorecard_head_after_write
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
        if len(command) > 1 and command[1].endswith("ci-hub/series/mirror.py"):
            report = publication_report(self.target)
            native = self.ledger_mirror_rc
            if self.scorecard_head_after_write is not None:
                self.fresh_head = self.scorecard_head_after_write
            if self.scorecard_rc or self.scorecard_head_after_write is not None:
                report["state"] = "derived-failed"
                detail = self.scorecard_stderr or "scorecard source checkout HEAD changed"
                report["scorecard"] = {"state": "failed", "error": detail}
                native = 1
            report.update(self.ledger_mirror_update)
            return completed(
                command, rc=native,
                stdout=json.dumps(report) + "\n" if self.ledger_mirror_rc == 0 else "",
                stderr=self.ledger_mirror_stderr,
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
            if self.real_source_git:
                working_directory = Path(
                    command[command.index("--working-directory") + 1]
                )
                self.systemd_checkout_head = subprocess.run(
                    ["git", "-C", str(working_directory), "rev-parse", "HEAD^{commit}"],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip()
            if self.systemd_launch_rc != 0:
                return completed(
                    command,
                    rc=self.systemd_launch_rc,
                    stderr=self.systemd_launch_stderr,
                )
            if self.typed_admission_refusal is not None:
                record = Path(command[command.index("--run-record") + 1])
                run_registry.publish_admission_result(
                    record,
                    state=run_registry.AdmissionState.REFUSED,
                    reason=run_registry.AdmissionRefusalReason(
                        self.typed_admission_refusal
                    ),
                    exit_code=3,
                )
            output = next(
                value.removeprefix("StandardOutput=append:")
                for value in command
                if value.startswith("StandardOutput=append:")
            )
            lines: list[str] = list(self.inner_log_lines)
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
            with Path(output).open("a") as log:
                log.write("\n".join(lines) + "\n")
            environment: dict[str, str] = {}
            for flag, value in zip(command, command[1:]):
                if flag == "--setenv" and "=" in value:
                    key, _, item = value.partition("=")
                    environment[key] = item
            if (
                self.write_service_result
                and self.final_validate_status is not None
                and (path := environment.get("VALIDATE_SERVICE_RESULT_PATH"))
            ):
                result = {
                    "schema_version": self.service_result_schema,
                    "commit": self.target,
                    "profile": "full",
                    "final_validate_status": self.final_validate_status,
                    "exit_code": self.actual_exit,
                    "executed_nodes": self.executed_nodes or 0,
                    "executed_tests": self.executed_tests,
                }
                if self.service_result_schema in (3, 4, 5):
                    result["selection_mode"] = "full"
                if self.service_result_schema in (2, 3, 4, 5):
                    result["scorecard_writeback"] = {"status": "completed"}
                if self.service_result_schema in (4, 5) and self.include_passed_tests:
                    result["passed_tests"] = self.passed_tests
                if self.service_result_schema == 5:
                    result["detail"] = self.detail
                Path(path).write_text(json.dumps(result) + "\n")
            if self.write_frozen_result and (frozen := environment.get("HERMIT_VALIDATE_LEDGER")):
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


class RetainedStepProfilesTest(unittest.TestCase):
    def command(
        self,
        *,
        unit: str = "validate-profile-a",
        repo: str = "rrnewton/hermit",
        environment: dict[str, str] | None = None,
    ) -> list[str]:
        return start_unit.build_systemd_command(
            root=Path("/immutable/tool"),
            state_root=Path("/canonical/state"),
            checkout=Path("/canonical/state/worktrees/validate/frozen-target"),
            target=SHA,
            agent="dev-hermit",
            unit=unit,
            record=Path("/canonical/state/ignored/validate/runs") / f"{unit}.json",
            log=Path("/canonical/state/ignored/validate") / f"{unit}.log",
            pr=3018,
            validate_args=["full"],
            wait=7200,
            hold=1200,
            child_deadline=3600,
            environment={"FLEET_REAL_GH": sys.executable, **(environment or {"HOME": "/home/test", "PATH": "/usr/bin:/bin"})},
            repo=repo,
        )

    def profile_values(self, command: list[str]) -> list[str]:
        return [item for item in command if item.startswith("RUN_NODE_PERF_DIR=")]

    def test_hermit_profiles_use_retained_state_root(self) -> None:
        command = self.command()
        expected = (
            "RUN_NODE_PERF_DIR=/canonical/state/ignored/validate/artifacts/"
            "validate-profile-a/dagrun-profiles"
        )
        self.assertEqual([expected], self.profile_values(command))
        self.assertEqual("--setenv", command[command.index(expected) - 1])

    def test_ambient_profile_path_cannot_replace_owned_artifact(self) -> None:
        command = self.command(environment={
            "HOME": "/home/test",
            "PATH": "/usr/bin:/bin",
            "RUN_NODE_PERF_DIR": "/foreign/checkout/profiles",
            "UNRELATED_ENVIRONMENT": "must-not-be-forwarded",
        })
        self.assertEqual(self.profile_values(self.command()), self.profile_values(command))
        self.assertEqual(1, len(self.profile_values(command)))
        self.assertNotIn("RUN_NODE_PERF_DIR=/foreign/checkout/profiles", command)
        self.assertFalse(any(item.startswith("UNRELATED_ENVIRONMENT=") for item in command))

    def test_distinct_units_have_distinct_profile_directories(self) -> None:
        first = self.profile_values(self.command(unit="validate-profile-a"))
        second = self.profile_values(self.command(unit="validate-profile-b"))
        self.assertEqual(1, len(first))
        self.assertEqual(1, len(second))
        self.assertNotEqual(first, second)
        self.assertEqual(
            ["RUN_NODE_PERF_DIR=/canonical/state/ignored/validate/artifacts/"
             "validate-profile-b/dagrun-profiles"],
            second,
        )

    def test_reverie_does_not_receive_hermit_profile_environment(self) -> None:
        ordinary = self.command(repo="rrnewton/reverie")
        ambient = self.command(repo="rrnewton/reverie", environment={
            "HOME": "/home/test",
            "PATH": "/usr/bin:/bin",
            "RUN_NODE_PERF_DIR": "/foreign/checkout/profiles",
        })
        self.assertEqual(ordinary, ambient)
        self.assertEqual([], self.profile_values(ordinary))
        self.assertIn("/immutable/tool/ci-hub/validate/reverie_safe_ci.py", ordinary)

    def test_profile_capture_preserves_ordinary_driver_and_limits(self) -> None:
        command = self.command()
        self.assertEqual(
            ["/usr/bin/env", "PR_NUMBER=3018", sys.executable,
             "/immutable/tool/ci-hub/validate/run_with_validate_deadline.py",
             "with-proxy", "./scripts/validate.rs", "full"],
            command[-7:],
        )
        self.assertIn("HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS=3239", command)
        for option, expected in (("--wait", "7200"), ("--hold", "1200"),
                                 ("--child-deadline", "3600"), ("--target", SHA)):
            self.assertEqual(expected, command[command.index(option) + 1])
        self.assertFalse(any(item.startswith("CI_DAG_JOBS=") for item in command))
        self.assertFalse(any(item.startswith("CARGO_BUILD_JOBS=") for item in command))


class HermitServiceResourcesTest(unittest.TestCase):
    def command(self, **overrides: object) -> list[str]:
        arguments = dict(
            root=Path("/immutable/tool"), state_root=Path("/canonical/state"),
            checkout=Path("/measured/hermit"), target=SHA, agent="fixture",
            unit="validate-startup-fixture", record=Path("/state/run.json"),
            log=Path("/state/run.log"), pr=None, validate_args=["quick"],
            wait=120, hold=1200, child_deadline=4800,
            environment={"HOME": "/home/test", "PATH": "/usr/bin:/bin", "FLEET_REAL_GH": sys.executable},
        )
        arguments.update(overrides)
        return start_unit.build_systemd_command(**arguments)

    def properties(self, command: list[str]) -> dict[str, str]:
        return dict(command[index + 1].split("=", 1)
                    for index, value in enumerate(command[:-1]) if value == "--property")

    def test_limits_are_initial_service_properties_before_any_tool_execution(self) -> None:
        command = self.command()
        properties = self.properties(command)
        self.assertEqual("0", properties["LimitCORE"])
        self.assertEqual("200%", properties["CPUQuota"])
        self.assertEqual("8589934592", properties["MemoryMax"])
        self.assertEqual("0", properties["MemorySwapMax"])
        self.assertEqual("4096", properties["TasksMax"])
        self.assertEqual("5520", properties["RuntimeMaxSec"])
        self.assertEqual("600", properties["TimeoutStopSec"])
        tool_index = command.index("/immutable/tool/ci-hub/ci-hub")
        for key in ("LimitCORE", "CPUQuota", "MemoryMax", "MemorySwapMax", "TasksMax", "RuntimeMaxSec"):
            self.assertLess(command.index(f"{key}={properties[key]}"), tool_index)
        self.assertEqual("4800", command[command.index("--child-deadline") + 1])
        self.assertIn("HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS=4200", command)
        self.assertFalse(any("DAGRUN_OUTER_MEMORY_MAX_BYTES" in value for value in command))

    def test_wall_encloses_effective_child_and_requested_queue_wait(self) -> None:
        for wait, requested_child, effective_child in [(1, 120, 120), (7200, 3600, 3600), (120, 7200, 4800)]:
            with self.subTest(wait=wait, child=requested_child):
                command = self.command(wait=wait, child_deadline=requested_child)
                properties = self.properties(command)
                self.assertEqual(effective_child, int(command[command.index("--child-deadline") + 1]))
                self.assertEqual(wait + effective_child + 600, int(properties["RuntimeMaxSec"]))
                self.assertGreater(int(properties["RuntimeMaxSec"]), wait + effective_child)
                self.assertEqual("600", properties["TimeoutStopSec"])

    def test_frozen_and_detached_current_tool_transports_keep_the_same_limits(self) -> None:
        ordinary = self.properties(self.command())
        for extra in [
            {"frozen_validate": True, "frozen_result": Path("/state/frozen.jsonl"),
             "driver_rust_toolchain": "nightly-2026-07-12-x86_64-unknown-linux-gnu"},
            {"unit_tool_prefix": ["/installed/bootstrap", "--run-current-tool"]},
        ]:
            with self.subTest(extra=extra):
                actual = self.properties(self.command(**extra))
                for key in ("LimitCORE", "CPUQuota", "MemoryMax", "MemorySwapMax", "TasksMax", "RuntimeMaxSec", "TimeoutStopSec"):
                    self.assertEqual(ordinary[key], actual[key])

    def test_reverie_keeps_its_existing_service_and_payload_bounds(self) -> None:
        command = self.command(repo="rrnewton/reverie", child_deadline=3600)
        properties = self.properties(command)
        self.assertEqual("0", properties["LimitCORE"])
        for key in ("CPUQuota", "MemoryMax", "MemorySwapMax", "TasksMax", "RuntimeMaxSec"):
            self.assertNotIn(key, properties)
        self.assertEqual("600", properties["TimeoutStopSec"])
        self.assertEqual("3600", command[command.index("--child-deadline") + 1])


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
        # These launcher fixtures model subprocesses and source trees. Rustup
        # resolution has separate real-context controls in test_rust_toolchain.
        toolchain_patch = mock.patch.object(
            start_unit.rust_toolchain, "selected_toolchain",
            return_value="nightly-2026-07-12-x86_64-unknown-linux-gnu",
        )
        self.toolchain = toolchain_patch.start()
        self.addCleanup(toolchain_patch.stop)
        self.runtime_roots: list[Path] = []
        real_prepare_runtime_root = start_unit.prepare_runtime_root

        def prepare_test_runtime_root(_environment: object) -> Path:
            runtime = real_prepare_runtime_root({"XDG_RUNTIME_DIR": "/tmp"})
            self.runtime_roots.append(runtime)
            return runtime

        runtime_patch = mock.patch.object(
            start_unit,
            "prepare_runtime_root",
            side_effect=prepare_test_runtime_root,
        )
        runtime_patch.start()
        self.addCleanup(runtime_patch.stop)
        self.environment = {
            "HOME": "/home/test",
            "PATH": "/usr/bin:/bin",
            "XDG_RUNTIME_DIR": str(self.root),
            "FLEET_REAL_GH": sys.executable,
        }
        route_patch = mock.patch.dict(os.environ, {"FLEET_REAL_GH": sys.executable})
        route_patch.start()
        self.addCleanup(route_patch.stop)
        # Keep launches from resolving or running the host's git: by default
        # there is no git.meta.real, so PATH is left unchanged. fake_host_gits()
        # swaps in temporary binaries for the tests of the substitution itself.
        self.original_probe_version = start_unit.unwrapped_git.probe_version
        for name, value in (
            ("REAL_GIT", self.root / "absent-host-bin/git.meta.real"),
            ("probe_version", lambda *_args, **_kwargs: ("git version fixture", "")),
        ):
            git_patch = mock.patch.object(start_unit.unwrapped_git, name, value)
            git_patch.start()
            self.addCleanup(git_patch.stop)

    def tearDown(self) -> None:
        start_unit.HOST_TMP_ROOT = self.original_host_tmp_root
        for runtime in self.runtime_roots:
            if runtime.is_symlink():
                runtime.unlink()
            else:
                shutil.rmtree(runtime, ignore_errors=True)
        self.temporary.cleanup()

    def historical_module(
        self,
        relative: str,
        name: str,
        *,
        run_registry_module: types.ModuleType | None = None,
    ) -> types.ModuleType:
        source = subprocess.run(
            [
                "git",
                "show",
                f"{PRE_POPULATION_WRITER_COMMIT}:{relative}",
            ],
            cwd=Path(__file__).resolve().parents[3],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        module = types.ModuleType(name)
        module.__file__ = str(Path(__file__).resolve().parents[3] / relative)
        previous = sys.modules.get("run_registry")
        previous_named = sys.modules.get(name)
        try:
            sys.modules[name] = module
            if run_registry_module is not None:
                sys.modules["run_registry"] = run_registry_module
            exec(compile(source, module.__file__, "exec"), module.__dict__)
        finally:
            if previous_named is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = previous_named
            if previous is None:
                sys.modules.pop("run_registry", None)
            else:
                sys.modules["run_registry"] = previous
        return module

    def invoke(
        self,
        extra: list[str] | None = None,
        *,
        run: start_unit.Runner | None = None,
        target: str = SHA,
        environment: dict[str, str] | None = None,
        unit: str = "validate-test",
    ) -> tuple[int, str, str]:
        out = io.StringIO()
        err = io.StringIO()
        log = self.root / ("run.log" if unit == "validate-test" else f"{unit}.log")
        argv = [
            "--checkout",
            str(self.checkout),
            "--agent",
            "hermit-test",
            "--target",
            target,
            "--unit",
            unit,
            "--log",
            str(log),
            *(extra or []),
        ]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                argv,
                run=run or self.fake,
                environment=self.environment if environment is None else environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )
        return rc, out.getvalue(), err.getvalue()

    def foreign_cleanup_lock_holder(
        self, kind: str, *, seconds: float, marker: Path | None = None
    ) -> subprocess.Popen[str]:
        """Start one exec'd process holding a real generation/population lock.

        ``exclusive-generation-then-population`` holds the exclusive generation
        side for ``seconds``, takes the population lock before releasing it,
        then holds population for ``seconds`` more. ``population-then-unlink``
        holds population for ``seconds`` and deletes ``marker`` before it
        releases, as a wrkslots removal clears its FINISH journal inside the
        same directory lock.
        """

        program = r"""
import fcntl
import os
import sys
import time
from pathlib import Path

kind, module_dir, raw_root, raw_seconds, raw_marker = sys.argv[1:]
root = Path(raw_root)
if kind == "generation":
    sys.path.insert(0, module_dir)
    import run_registry
    with run_registry.run_writer_generation_lock(root):
        print("ready", flush=True)
        time.sleep(float(raw_seconds))
elif kind == "exclusive-generation":
    # The side destructive cleanup takes; it excludes a launcher's shared side.
    sys.path.insert(0, module_dir)
    import run_registry
    with run_registry.run_writer_generation_lock(root, exclusive=True):
        print("ready", flush=True)
        time.sleep(float(raw_seconds))
elif kind == "population":
    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        print("ready", flush=True)
        time.sleep(float(raw_seconds))
    finally:
        os.close(descriptor)
elif kind == "exclusive-generation-then-population":
    sys.path.insert(0, module_dir)
    import run_registry
    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        with run_registry.run_writer_generation_lock(root, exclusive=True):
            print("ready", flush=True)
            time.sleep(float(raw_seconds))
            fcntl.flock(descriptor, fcntl.LOCK_EX)
        time.sleep(float(raw_seconds))
    finally:
        os.close(descriptor)
elif kind == "population-then-unlink":
    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        print("ready", flush=True)
        time.sleep(float(raw_seconds))
        os.unlink(raw_marker)
    finally:
        os.close(descriptor)
else:
    raise SystemExit(f"unknown lock kind: {kind}")
"""
        holder = subprocess.Popen(
            [
                sys.executable,
                "-c",
                program,
                kind,
                str(Path(start_unit.run_registry.__file__).resolve().parent),
                str(self.root),
                str(seconds),
                "" if marker is None else str(marker),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        def stop_holder() -> None:
            if holder.poll() is None:
                holder.terminate()
                try:
                    holder.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    holder.kill()
                    holder.wait(timeout=2)
            if holder.stdout is not None:
                holder.stdout.close()
            if holder.stderr is not None:
                holder.stderr.close()

        self.addCleanup(stop_holder)
        assert holder.stdout is not None
        self.assertEqual("ready", holder.stdout.readline().strip())
        self.assertIsNone(holder.poll())
        return holder

    def invoke_with_foreign_cleanup_lock(
        self,
        kind: str,
        *,
        holder_seconds: float,
        wait_seconds: float,
    ) -> tuple[int, str, str, subprocess.Popen[str]]:
        real_finalize = start_unit.finalize_commit_status_publication
        holder: subprocess.Popen[str] | None = None

        def hold_after_publication(*args: object, **kwargs: object):
            nonlocal holder
            updated = real_finalize(*args, **kwargs)
            holder = self.foreign_cleanup_lock_holder(
                kind, seconds=holder_seconds
            )
            return updated

        with (
            mock.patch.object(
                start_unit,
                "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
                PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
            ),
            mock.patch.object(
                start_unit,
                "SAME_LAUNCH_CLEANUP_LOCK_WAIT_SECONDS",
                wait_seconds,
            ),
            mock.patch.object(
                start_unit, "SAME_LAUNCH_CLEANUP_LOCK_POLL_SECONDS", 0.005
            ),
            mock.patch.object(
                start_unit,
                "finalize_commit_status_publication",
                side_effect=hold_after_publication,
            ),
        ):
            rc, output, error = self.invoke(["--", "full"])

        assert holder is not None
        return rc, output, error, holder

    def assert_record_storage_removed(self) -> None:
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertIn(field, record)
        for field in ("checkout", "cargo_home", "runtime_root"):
            path = Path(record[field])
            self.assertFalse(path.exists() or path.is_symlink(), path)

    def assert_record_storage_retained(self) -> None:
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        for field in ("checkout", "cargo_home", "runtime_root"):
            self.assertTrue(Path(record[field]).is_dir(), field)
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertNotIn(field, record)

    @mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None)
    def test_measured_toolchain_reaches_unit_and_retained_log(self, _pane) -> None:
        environment = {**self.environment, "RUSTUP_TOOLCHAIN": "stable"}
        rc, _output, error = self.invoke(
            ["--in-place", "--", "quick"], environment=environment
        )
        self.assertEqual(0, rc, error)
        self.assertEqual(self.checkout, self.toolchain.call_args.args[0])
        selected = self.toolchain.return_value
        commands = [c for c in self.fake.commands if c[0] == "systemd-run"]
        self.assertEqual(1, len(commands))
        self.assertIn(f"RUSTUP_TOOLCHAIN={selected}", commands[0])
        self.assertNotIn("RUSTUP_TOOLCHAIN=stable", commands[0])
        self.assertIn("RUSTUP_AUTO_INSTALL=0", commands[0])
        record = run_registry.read_record(run_registry.record_path(self.root, "validate-test"))
        log = Path(record["log"]).read_text()
        self.assertIn(selected, log)
        self.assertIn(str(self.checkout / "rust-toolchain.toml"), log)

    def test_toolchain_refusal_precedes_runtime_cache_and_unit(self) -> None:
        self.toolchain.side_effect = start_unit.rust_toolchain.ToolchainRefused(
            "configured Hermit toolchain is unavailable"
        )
        rc, _output, error = self.invoke(["--in-place", "--", "quick"])
        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertIn("configured Hermit toolchain is unavailable", error)
        self.assertFalse(any(c[0] in {"mktemp", "systemd-run"} for c in self.fake.commands))
        self.assertFalse(run_registry.record_path(self.root, "validate-test").exists())

    @mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None)
    def test_frozen_driver_uses_current_tool_hermit_toolchain(self, _pane) -> None:
        measured = "nightly-2026-06-01-x86_64-unknown-linux-gnu"
        driver = "nightly-2026-07-12-x86_64-unknown-linux-gnu"
        self.toolchain.side_effect = [measured, driver]
        rc, _output, error = self.invoke(["--frozen-validate", "--", "quick"])
        self.assertEqual(0, rc, error)
        self.assertIsNotNone(self.fake.fresh)
        self.assertNotEqual(self.checkout, self.fake.fresh)
        self.assertEqual(self.fake.fresh, self.toolchain.call_args_list[0].args[0])
        self.assertEqual(self.root / "hermit", self.toolchain.call_args.args[0])
        command = next(c for c in self.fake.commands if c[0] == "systemd-run")
        self.assertIn(f"RUSTUP_TOOLCHAIN={measured}", command)
        self.assertNotIn(f"RUSTUP_TOOLCHAIN={driver}", command)
        rust_script = command.index("rust-script")
        self.assertEqual(
            ["--force", "--toolchain", driver], command[rust_script + 1:rust_script + 4]
        )
        record = run_registry.read_record(run_registry.record_path(self.root, "validate-test"))
        self.assertFalse(record["qualifying_receipt"])
        log = Path(record["log"]).read_text()
        self.assertIn(f"Rust toolchain {driver} selected from {self.root / 'hermit/rust-toolchain.toml'}", log)
        self.assertIn(f"measured Rust toolchain {measured} selected from {self.fake.fresh / 'rust-toolchain.toml'}", log)

    @mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None)
    def test_default_deadline_encloses_committed_quick_nodes_and_cleanup(self, _pane) -> None:
        rc, _output, error = self.invoke(["--in-place", "--", "quick"])
        self.assertEqual(0, rc, error)
        record = run_registry.read_record(run_registry.record_path(self.root, "validate-test"))
        whole = record["hermit_run_timeout_seconds"]
        child = record["validate_lock_child_deadline_seconds"]
        dag = json.loads((start_unit.ROOT / "hermit/ci/dag/validate.json").read_text())
        quick = [s for s in dag["steps"] if "quick" in s.get("labels", [])]
        self.assertTrue(quick)
        self.assertLess(max(s["timeout"] for s in quick), whole)
        self.assertLess(whole + max(60, whole // 10), child)
        self.assertLessEqual(whole, start_unit.HERMIT_MAX_RUN_TIMEOUT_SECONDS)
        self.assertLessEqual(child, start_unit.HERMIT_MAX_CHILD_DEADLINE_SECONDS)

    def write_validation_service_schema(self, version: int) -> None:
        schema_path = self.checkout / start_unit.service_result.SCHEMA_RELATIVE_PATH
        schema_path.parent.mkdir(parents=True, exist_ok=True)
        schema = {
            "schema_version": version,
            "fields": list(
                {
                    1: start_unit.service_result.HISTORICAL_FIELD_NAMES,
                    2: start_unit.service_result.WRITEBACK_FIELD_NAMES,
                    3: start_unit.service_result.SELECTION_FIELD_NAMES,
                    4: start_unit.service_result.TEST_COUNTS_FIELD_NAMES,
                    5: start_unit.service_result.FIELD_NAMES,
                }[version]
            ),
            "outcomes": (
                start_unit.service_result.HISTORICAL_EXPECTED_OUTCOMES
                if version == 1
                else start_unit.service_result.EXPECTED_OUTCOMES
            ),
        }
        if version in (2, 3, 4, 5):
            schema["scorecard_writeback"] = (
                start_unit.service_result.EXPECTED_SCORECARD_WRITEBACK
            )
        schema_path.write_text(json.dumps(schema))

    def authority_fixture(
        self,
        bootstrap: Path,
        *,
        root: Path | None = None,
        state_root: Path | None = None,
        target_root: Path | None = None,
        sealed: bool = True,
        update: dict[str, object] | None = None,
        remove: set[str] | None = None,
    ) -> AuthorityFixture:
        sequence = getattr(self, "_authority_sequence", 0) + 1
        self._authority_sequence = sequence
        if root is None:
            root = self.root / f"authority-tool-{sequence}"
            (root / "ci-hub").mkdir(parents=True)
            (root / "ci-hub/authority-marker").write_text("authorized\n")
            make_tree_read_only(root)
        if target_root is None:
            target_sha = str((update or {}).get("parent_sha", SHA))
            target_root = (
                self.root / f"authority-cache-{sequence}" / "trees" / target_sha
            )
            target_root.mkdir(parents=True)
            make_tree_read_only(target_root)
        fixture = AuthorityFixture(
            root,
            state_root or self.root,
            target_root,
            bootstrap_sha256=hashlib.sha256(bootstrap.read_bytes()).hexdigest(),
            sealed=sealed,
            update=update,
            remove=remove,
        )
        self.addCleanup(fixture.close)
        return fixture

    def split_tool_root(self) -> Path:
        tool_root = Path(f"{self.root}-tool")
        self.addCleanup(shutil.rmtree, tool_root, True)
        for relative in (
            "ci-hub/ci-hub",
            "ci-hub/ledger/validate_rows.py",
            "hermit/agent-utils/rs/dagrun/Cargo.toml",
        ):
            path = tool_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"tool fixture: {relative}\n")
        return tool_root

    @staticmethod
    def tree_bytes(root: Path) -> dict[str, bytes]:
        return {
            str(path.relative_to(root)): path.read_bytes()
            for path in root.rglob("*")
            if path.is_file()
        }

    @staticmethod
    def worktree_bytes(root: Path) -> dict[str, bytes]:
        return {
            str(path.relative_to(root)): path.read_bytes()
            for path in root.rglob("*")
            if path.is_file() and ".git" not in path.relative_to(root).parts
        }

    @staticmethod
    def git_output(checkout: Path, *arguments: str) -> str:
        return subprocess.run(
            ["git", "-C", str(checkout), *arguments],
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    def initialize_real_materialize_source(self) -> str:
        """Make HEAD differ from a still-present exact target in a clean real repo."""

        subprocess.run(
            ["git", "init", "-b", "caller-branch", str(self.checkout)],
            check=True,
            capture_output=True,
            text=True,
        )
        for key, value in (("user.name", "Test User"), ("user.email", "test@example.com")):
            subprocess.run(
                ["git", "-C", str(self.checkout), "config", key, value],
                check=True,
            )
        runner_crate = self.checkout / "agent-utils/rs/dagrun/Cargo.toml"
        runner_crate.parent.mkdir(parents=True)
        runner_crate.write_text("[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n")
        runner = self.checkout / "agent-utils/common/bin/dagrun"
        runner.parent.mkdir(parents=True)
        runner.write_text("#!/bin/sh\n")
        runner.chmod(0o755)
        marker = self.checkout / "materialized-version"
        marker.write_text("exact target\n")
        subprocess.run(
            ["git", "-C", str(self.checkout), "add", "."],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.checkout), "commit", "-m", "exact target"],
            check=True,
            capture_output=True,
            text=True,
        )
        target = self.git_output(self.checkout, "rev-parse", "HEAD^{commit}").strip()
        marker.write_text("caller head\n")
        subprocess.run(
            ["git", "-C", str(self.checkout), "add", "materialized-version"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.checkout), "commit", "-m", "caller head"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.fake.target = target
        self.fake.fresh_head = target
        self.fake.source_target = target
        self.fake.real_source_git = True
        return target

    def initialize_hostile_git_redirect(
        self, *, dirty_source: bool
    ) -> tuple[str, str, dict[str, str]]:
        """Create a repository-selector environment that lies about source state."""
        subprocess.run(
            ["git", "init", "-b", "caller-branch", str(self.checkout)],
            check=True,
            capture_output=True,
            text=True,
        )
        for key, value in (("user.name", "Test User"), ("user.email", "test@example.com")):
            subprocess.run(
                ["git", "-C", str(self.checkout), "config", key, value],
                check=True,
            )
        runner_crate = self.checkout / "agent-utils/rs/dagrun/Cargo.toml"
        runner_crate.parent.mkdir(parents=True)
        runner_crate.write_text('[package]\nname = "fixture"\nversion = "0.0.0"\n')
        runner = self.checkout / "agent-utils/common/bin/dagrun"
        runner.parent.mkdir(parents=True)
        runner.write_text("#!/bin/sh\n")
        runner.chmod(0o755)
        subprocess.run(["git", "-C", str(self.checkout), "add", "."], check=True)
        subprocess.run(
            ["git", "-C", str(self.checkout), "commit", "-m", "source base"],
            check=True,
            capture_output=True,
            text=True,
        )
        source_head = self.git_output(
            self.checkout, "rev-parse", "HEAD^{commit}"
        ).strip()
        subprocess.run(
            [
                "git",
                "-C",
                str(self.checkout),
                "commit",
                "--allow-empty",
                "-m",
                "exact target",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        target = self.git_output(self.checkout, "rev-parse", "HEAD^{commit}").strip()
        wrong = self.root / "hostile-git-dir"
        subprocess.run(
            ["git", "clone", "-q", str(self.checkout), str(wrong)], check=True
        )
        subprocess.run(
            ["git", "-C", str(self.checkout), "checkout", "-q", source_head],
            check=True,
        )
        if dirty_source:
            dirty_payload = "scorecard\nhostile-index-content\n"
            (self.checkout / "SCORECARD.md").write_text(dirty_payload)
            (wrong / "SCORECARD.md").write_text(dirty_payload)
            subprocess.run(["git", "-C", str(wrong), "add", "SCORECARD.md"], check=True)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(wrong),
                    "-c",
                    "user.name=Test User",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-m",
                    "hostile matching index",
                ],
                check=True,
                capture_output=True,
                text=True,
            )
        hostile = {
            **self.environment,
            "GIT_DIR": str(wrong / ".git"),
            "GIT_WORK_TREE": str(self.checkout),
            "GIT_INDEX_FILE": str(wrong / ".git/index"),
        }
        self.fake.real_source_git = True
        self.fake.target = target
        self.fake.fresh_head = target
        return source_head, target, hostile

    def caller_git_state(self) -> dict[str, object]:
        return {
            "branch": self.git_output(
                self.checkout, "symbolic-ref", "--short", "HEAD"
            ),
            "head": self.git_output(self.checkout, "rev-parse", "HEAD^{commit}"),
            "status": self.git_output(
                self.checkout, "status", "--porcelain=v1", "--untracked-files=all"
            ),
            "index": self.git_output(self.checkout, "ls-files", "--stage"),
            "cached_diff": self.git_output(
                self.checkout, "diff", "--cached", "--binary"
            ),
            "worktree": self.worktree_bytes(self.checkout),
        }

    def assert_canonical_readers_use_split_roots(self, tool_root: Path) -> None:
        ledger_suffix = [
            str(tool_root / "ci-hub/ledger/validate_rows.py"),
            "rows",
            "--preserve-admission",
        ]
        readers = [
            (command, cwd)
            for command, cwd in self.fake.command_cwds
            if command[1:3] == ["validate-status", "--sha"]
            or command[-3:] == ledger_suffix
        ]
        self.assertEqual(2, len(readers))
        self.assertEqual(str(tool_root / "ci-hub/ci-hub"), readers[0][0][0])
        self.assertEqual(
            ledger_suffix, readers[1][0][-3:]
        )
        self.assertEqual([self.root, self.root], [cwd for _, cwd in readers])

    def managed_cleanup_record(
        self,
        suffix: str,
        *,
        checkout: Path | None = None,
        generation: int | None = 7,
        state: str = "completed",
        temporary: bool = True,
    ) -> tuple[Path, Path]:
        fresh = checkout or (
            self.root / "worktrees/validate" / f"validate-fresh-{suffix}"
        )
        fresh.mkdir(parents=True, exist_ok=True)
        git_file = fresh / ".git"
        if not git_file.exists():
            git_file.write_text("gitdir: shared/worktrees/fixture\n")
        record_path = self.root / "ignored/validate/runs" / f"validate-{suffix}.json"
        record: dict[str, object] = {
            "schema_version": 1,
            "unit": f"validate-{suffix}.service",
            "checkout": str(fresh),
            "source_checkout": str(self.checkout),
            "temporary_checkout": temporary,
            "repo": "rrnewton/hermit",
            "state": state,
            "exit_code": 0,
            "final_validate_status": "PASSED",
        }
        if generation is not None:
            record.update(
                wrkslots_slot=fresh.name,
                wrkslots_generation=generation,
            )
        start_unit.run_registry.write_record(record_path, record)
        return fresh, record_path

    def in_place_cleanup_record(
        self,
        suffix: str,
        *,
        temporary: object = False,
        include_temporary: bool = True,
        current: bool = True,
        state: str = "completed",
        admitted: bool = False,
    ) -> tuple[Path, Path, Path]:
        checkout = (
            self.root
            / "worktrees/slots/series-rows/ignored/series-rows/validation-prereq-hermit"
        )
        checkout.mkdir(parents=True, exist_ok=True)
        (checkout / "preserved").write_text("series rows\n")
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes"
            / f"validate-cargo-{suffix}"
        )
        cargo_home.mkdir(parents=True)
        (cargo_home / "preserved").write_text("cargo cache\n")
        record_path = (
            self.root / "ignored/validate/runs" / f"validate-{suffix}.json"
        )
        record: dict[str, object] = {
            "schema_version": 1,
            "unit": f"validate-{suffix}.service",
            "checkout": str(checkout),
            "source_checkout": str(checkout),
            "cargo_home": str(cargo_home),
            "repo": "rrnewton/hermit",
            "state": state,
            "exit_code": 0,
            "final_validate_status": "PASSED",
        }
        if include_temporary:
            record["temporary_checkout"] = temporary
        if current:
            record.update(
                kind="validate",
                target=SHA,
                log=str(self.root / "ignored/validate" / f"validate-{suffix}.log"),
                agent="series-rows",
                started_at="2026-09-14T07:00:00+00:00",
                finished_at="2026-09-14T07:01:00+00:00",
                producer=start_unit.run_registry.PRODUCER,
                admission="ci-hub validate-lock",
                pane_role="observer-only",
                parent_checkout_head=None,
                validate_lock_child_deadline_seconds=3600,
                e2e_result_root=str(
                    self.root / "ignored/validate/artifacts" / suffix / "e2e"
                ),
                safe_ci_dag_runner_log_dir=str(
                    self.root
                    / "ignored/validate/artifacts"
                    / suffix
                    / "safe-ci-dag-runner"
                ),
                hermit_run_timeout_seconds=3239,
                result="success" if state == "completed" else "unknown",
                detail=(
                    "the validation result was not recorded"
                    if state == "unknown"
                    else None
                ),
            )
            if admitted:
                record["admission_result"] = {
                    "recorded_at": "2026-09-14T07:00:01+00:00",
                    "run_number": 1800,
                    "state": "admitted",
                }
        else:
            record["producer"] = start_unit.LEGACY_IN_PLACE_PRODUCER
            record["agent"] = "series-rows"
        if current and include_temporary and type(temporary) is bool:
            start_unit.run_registry.write_record(record_path, record)
        else:
            record_path.parent.mkdir(parents=True, exist_ok=True)
            record_path.write_text(json.dumps(record, sort_keys=True) + "\n")
        return checkout, cargo_home, record_path

    @staticmethod
    def filesystem_snapshot(root: Path) -> dict[str, tuple[object, ...]]:
        snapshot: dict[str, tuple[object, ...]] = {}
        for path in sorted(root.rglob("*")):
            relative = path.relative_to(root).as_posix()
            metadata = path.lstat()
            if path.is_symlink():
                payload: object = os.readlink(path)
            elif path.is_file():
                payload = path.read_bytes()
            else:
                payload = tuple(sorted(child.name for child in path.iterdir()))
            snapshot[relative] = (
                stat.S_IFMT(metadata.st_mode),
                metadata.st_ino,
                metadata.st_size,
                metadata.st_mtime_ns,
                payload,
            )
        return snapshot

    def create_journal_report(
        self,
        *,
        prospective_slot: str,
        state: str = "dead-incomplete-create",
        checkout: Path | None = None,
        observed_head: str = SHA,
    ) -> str:
        slot_path = self.root / "worktrees/slots/series-rows"
        created_path = checkout or slot_path
        journal = self.root / "worktrees/ACTIVE.devbig014.journal"
        journal.parent.mkdir(parents=True, exist_ok=True)
        if not journal.exists():
            journal.write_bytes(b'{"kind":"create","slot":"series-rows"}\n')
        metadata = journal.stat(follow_symlinks=False)
        process_state = state.removesuffix("-incomplete-create")
        planned = {
            "branch": "series-rows-parent-20260913",
            "destination": created_path.relative_to(self.root).as_posix(),
            "landed_ref": "refs/remotes/origin/main",
            "name": "parent",
            "remote": "origin",
            "remote_url_sha256": "d" * 64,
            "repository": ".",
            "start_point": SHA,
        }
        created = {
            **planned,
            "containing_remote_refs": [],
            "head": SHA,
            "path": planned["destination"],
            "vcs": "git",
        }
        created.pop("destination")
        observed = {
            "branch": created["branch"],
            "dirty": True,
            "head": observed_head,
            "name": created["name"],
            "path": created["path"],
        }
        return json.dumps(
            {
                "blocking": False,
                "journals": [
                    {
                        "agent": "series-rows",
                        "checkouts": [observed],
                        "coordinator": "recorded process generation is dead",
                        "coordinator_state": process_state,
                        "created": 1,
                        "dirty_checkouts": ["parent"],
                        "journal": str(journal),
                        "journal_identity": {
                            "device": metadata.st_dev,
                            "inode": metadata.st_ino,
                            "sha256": hashlib.sha256(journal.read_bytes()).hexdigest(),
                            "size": metadata.st_size,
                        },
                        "machine": "devbig014",
                        "owner": "recorded process generation is dead",
                        "owner_state": process_state,
                        "planned": 1,
                        "registry_state": "absent",
                        "slot": "series-rows",
                        "slot_path": str(slot_path),
                        "slot_type": "agent",
                        "state": state,
                    }
                ],
                "requested": {
                    "agent": f"validate-{prospective_slot}",
                    "slot": prospective_slot,
                    "slot_path": str(
                        self.root / "worktrees/validate" / prospective_slot
                    ),
                    "slot_type": "validate",
                },
                "schema": 2,
            },
            sort_keys=True,
        )

    def ownerless_cleanup_record(self, suffix: str) -> tuple[Path, Path]:
        fresh = self.root / "ignored" / f"validate-fresh-{suffix}"
        fresh.mkdir(parents=True)
        record_path = self.root / "ignored/validate/runs" / f"validate-{suffix}.json"
        start_unit.run_registry.write_record(
            record_path,
            {
                "schema_version": 1,
                "unit": f"validate-{suffix}.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )
        return fresh, record_path

    def frozen_cleanup_record(self, suffix: str) -> tuple[Path, Path]:
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / f"validate-fresh-{suffix}"
        )
        (fresh / ".git").mkdir(parents=True)
        self.addCleanup(
            shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True
        )
        record_path = (
            self.root / "ignored/validate/runs" / f"validate-{suffix}.json"
        )
        start_unit.run_registry.write_record(
            record_path,
            {
                "schema_version": 1,
                "unit": f"validate-{suffix}.service",
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
        return fresh, record_path

    def cleanup_route_fixture(
        self, route: str, suffix: str
    ) -> tuple[Path, Path, dict[str, object]]:
        if route == "managed":
            target, record = self.managed_cleanup_record(suffix)
        elif route == "ownerless":
            target, record = self.ownerless_cleanup_record(suffix)
        elif route == "frozen":
            target, record = self.frozen_cleanup_record(suffix)
        elif route == "direct":
            target, record = self.managed_cleanup_record(suffix)
            self.fake.fresh = target
            self.fake.registered_validate_slots.add(
                (target.name, self.fake.wrkslots_generation)
            )
        elif route == "cargo":
            _checkout, target, record = self.in_place_cleanup_record(
                suffix, temporary=False, current=True
            )
        else:
            raise AssertionError(route)
        return target, record, start_unit.read_cleanup_record_object(record)

    def run_cleanup_route(
        self,
        route: str,
        target: Path,
        record: Path,
        payload: dict[str, object],
        run: start_unit.Runner | None = None,
    ) -> bool:
        runner = run or self.fake
        if route in {"managed", "ownerless", "frozen"}:
            report = start_unit.sweep_completed_checkouts(
                self.root, run=runner, tool_root=self.root
            )
            return str(target) in report["removed"]
        if route == "direct":
            removed, _archived, _reason = start_unit.cleanup_recorded_checkout(
                self.root,
                payload,
                record_path=record,
                unit=record.stem,
                run=runner,
                tool_root=self.root,
                wrkslots=start_unit.WrkslotsIdentity(
                    target.name, self.fake.wrkslots_generation
                ),
            )
            return removed
        if route == "cargo":
            removed, _reason = start_unit.cleanup_recorded_cargo_home(
                self.root,
                payload,
                record_path=record,
                run=runner,
                tool_root=self.root,
            )
            return removed
        raise AssertionError(route)

    def assert_provider_parent_replacement_is_serialized(
        self,
        fresh: Path,
        record: Path,
        *,
        action: str,
    ) -> None:
        """A replacement runs/ parent must not admit a canonical writer."""

        runs = record.parent
        displaced = runs.with_name(f"{runs.name}.{fresh.name}.displaced")
        replacement_lock_was_free = False
        writer_was_rejected = False

        def run(command: list[str], **kwargs: object):
            nonlocal replacement_lock_was_free, writer_was_rejected
            if wrkslots_action(command, action):
                os.rename(runs, displaced)
                shutil.copytree(displaced, runs)
                replacement = runs / record.name
                with start_unit.run_registry.exclusive_record(
                    replacement, blocking=False
                ):
                    replacement_lock_was_free = True
                try:
                    start_unit.run_registry.update_record(
                        replacement,
                        blocking=False,
                        concurrent_writer="replacement-parent",
                    )
                except start_unit.run_registry.RecordLocked:
                    writer_was_rejected = True
                    raise
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        durable = start_unit.run_registry.read_record(runs / record.name)
        self.assertTrue(replacement_lock_was_free)
        self.assertTrue(writer_was_rejected)
        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertNotIn("concurrent_writer", durable)
        self.assertNotIn("checkout_removed_at", durable)
        self.assertTrue(
            any(row.get("cleanup_failed") is True for row in report["retained"]),
            report,
        )

    def current_unknown_cleanup_record(
        self, suffix: str, admission_result: dict[str, object] | None
    ) -> tuple[Path, dict[str, object]]:
        fresh = self.root / "worktrees/validate" / f"validate-fresh-{suffix}"
        fresh.mkdir(parents=True)
        record = {
            "schema_version": start_unit.run_registry.SCHEMA_VERSION,
            "kind": "validate",
            "state": "unknown",
            "result": "unknown",
            "detail": "the validation result was not recorded",
            "unit": f"validate-{suffix}.service",
            "target": SHA,
            "repo": "rrnewton/hermit",
            "checkout": str(fresh),
            "source_checkout": str(self.checkout),
            "temporary_checkout": True,
            "wrkslots_slot": fresh.name,
            "wrkslots_generation": 7,
            "cargo_home": str(
                self.root / "ignored/validate/cargo-homes" / f"validate-cargo-{suffix}"
            ),
            "log": str(self.root / "ignored/validate" / f"validate-{suffix}.log"),
            "agent": "cleanup-test",
            "started_at": "2026-09-04T12:00:00+00:00",
            "finished_at": "2026-09-04T12:01:00+00:00",
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
        }
        if admission_result is not None:
            record["admission_result"] = admission_result
        return fresh, record

    def removed_temporary_paths(self) -> list[Path]:
        return [
            Path(command[-1])
            for command in self.fake.commands
            if command[:2] == ["rm", "-rf"]
        ]

    def private_cargo_home(self) -> Path:
        return (
            self.root
            / "ignored/validate/cargo-homes/validate-cargo-abcd1234"
        )

    def recoverable_runtime_root(self) -> Path:
        runtime = Path(tempfile.mkdtemp(prefix="v", dir="/tmp"))
        self.runtime_roots.append(runtime)
        return runtime

    def assert_main_checkout_and_cargo_removed(
        self, checkout: Path | None = None
    ) -> None:
        selected_checkout = checkout if checkout is not None else self.fake.fresh
        self.assertIsNotNone(selected_checkout)
        self.assertEqual(
            [str(selected_checkout), str(self.private_cargo_home())],
            self.fake.removed,
        )

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
        self.assertIn("HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS=4200", systemd)
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
        self.assertEqual("codex/fixture-branch", record["branch"])
        self.assertEqual(4800, record["validate_lock_child_deadline_seconds"])
        self.assertEqual(4200, record["hermit_run_timeout_seconds"])

    def test_launch_carries_and_records_cpu_placement(self) -> None:
        # https://github.com/rrnewton/hermit/issues/3265: an explicit override
        # keeps this independent of the test host's PMU layout.
        environment = {**self.environment, "DEV_HERMIT_EXCLUDE_CPUS": "0"}
        rc, _output, error = self.invoke(["--", "full"], environment=environment)

        self.assertEqual(0, rc, error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        wrapper = systemd[: systemd.index("validate-lock")]
        allowed = [
            value.removeprefix("AllowedCPUs=")
            for flag, value in zip(wrapper, wrapper[1:])
            if flag == "--property" and value.startswith("AllowedCPUs=")
        ]
        self.assertEqual(1, len(allowed), wrapper)
        allowed_cpus = start_unit.cpu_placement.parse_cpu_list(allowed[0])
        self.assertIsNotNone(allowed_cpus)
        assert allowed_cpus is not None
        self.assertNotIn(0, allowed_cpus)
        self.assertTrue(allowed_cpus)
        self.assertIn(f"HERMIT_CI_ALLOWED_CPUS={allowed[0]}", wrapper)
        self.assertIn("HERMIT_CI_EXCLUDED_CPUS=0", wrapper)
        self.assertIn("HERMIT_CI_CPU_PLACEMENT_SOURCE=override", wrapper)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        placement = record["cpu_placement"]
        self.assertEqual("override", placement["source"])
        self.assertEqual("0", placement["excluded_cpus"])
        self.assertEqual(allowed[0], placement["allowed_cpus"])
        self.assertEqual("DEV_HERMIT_EXCLUDE_CPUS", placement["override_env"])
        self.assertIn(
            "validate-run: cpu placement source=override excluded=0 ",
            (self.root / "run.log").read_text(encoding="utf-8"),
        )

    def fake_host_gits(self) -> tuple[Path, Path, Path]:
        """A fake wrapper `git` beside a fake git.meta.real, and a plain git.

        Temporary scripts only; the host's /usr/local/bin is never run.
        """
        local = self.root / "fake-local-bin"
        plain = self.root / "fake-plain-bin"
        for directory, name, version in (
            (local, "git", "git version 2.53.0-Meta"),
            (local, "git.meta.real", "git version 2.53.0-Meta"),
            (plain, "git", "git version 2.52.0"),
        ):
            directory.mkdir(exist_ok=True)
            script = directory / name
            script.write_text(f"#!/bin/sh\necho '{version}'\n", encoding="utf-8")
            script.chmod(0o755)
        for name, value in (
            ("REAL_GIT", local / "git.meta.real"),
            ("probe_version", self.original_probe_version),
        ):
            git_patch = mock.patch.object(start_unit.unwrapped_git, name, value)
            git_patch.start()
            self.addCleanup(git_patch.stop)
        return local, local / "git.meta.real", plain

    def unit_path_value(self, command: list[str]) -> str:
        values = [item.removeprefix("PATH=") for item in command if item.startswith("PATH=")]
        self.assertEqual(1, len(values), command)
        return values[0]

    def unwrapped_git_directory(self) -> Path:
        # Inside the launch's private runtime root, which every refusal,
        # completion and orphan-recovery path removes.
        self.assertTrue(self.runtime_roots)
        return self.runtime_roots[-1] / "unwrapped-git"

    def launched_systemd_command(self) -> list[str]:
        return next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )

    def record_git_substitution(self) -> dict[str, object]:
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        return record["git_substitution"]

    def test_launch_runs_unwrapped_git_when_path_git_is_the_wrapper(self) -> None:
        local, real, _plain = self.fake_host_gits()
        environment = {**self.environment, "PATH": f"{local}:/usr/bin:/bin"}
        # The observer pane is created after systemd-run and before the run
        # completes, so this is the directory the running unit sees.
        seen: dict[str, object] = {}

        def observe_directory(*_args: object, **_kwargs: object) -> None:
            directory = self.unwrapped_git_directory()
            seen["entries"] = sorted(entry.name for entry in directory.iterdir())
            seen["link"] = os.readlink(directory / "git")
            return None

        with mock.patch.object(
            start_unit.pane_owner, "create_pane", side_effect=observe_directory
        ):
            rc, _output, error = self.invoke(["--", "full"], environment=environment)

        self.assertEqual(0, rc, error)
        directory = self.unwrapped_git_directory()
        self.assertEqual(
            f"{directory}:{local}:/usr/bin:/bin",
            self.unit_path_value(self.launched_systemd_command()),
        )
        self.assertEqual({"entries": ["git"], "link": str(real)}, seen)
        # Removed with the runtime root when the run completed.
        self.assertFalse(directory.exists())
        log = (self.root / "run.log").read_text(encoding="utf-8")
        self.assertIn("validate-run: git substitution APPLIED: ", log)
        self.assertIn(f'"effective_git": "{real}"', log)
        self.assertIn('"effective_version": "git version 2.53.0-Meta"', log)
        self.assertIn(f'"resolved_git": "{local / "git"}"', log)
        # The run handle carries the same decision, machine-readably.
        substitution = self.record_git_substitution()
        self.assertEqual(True, substitution["applied"])
        self.assertEqual("applied", substitution["code"])
        self.assertEqual(str(directory), substitution["directory"])
        self.assertEqual(str(real), substitution["real_git"])
        self.assertEqual(str(real), substitution["effective_git"])
        self.assertEqual(str(local / "git"), substitution["resolved_git"])
        self.assertEqual("git version 2.53.0-Meta", substitution["effective_version"])

    def test_launch_leaves_path_unchanged_when_git_is_not_the_wrapper(self) -> None:
        local, _real, plain = self.fake_host_gits()
        path = f"{plain}:{local}:/usr/bin:/bin"
        environment = {**self.environment, "PATH": path}
        rc, _output, error = self.invoke(["--", "full"], environment=environment)

        self.assertEqual(0, rc, error)
        self.assertEqual(path, self.unit_path_value(self.launched_systemd_command()))
        self.assertFalse(self.unwrapped_git_directory().exists())
        log = (self.root / "run.log").read_text(encoding="utf-8")
        self.assertIn("validate-run: git substitution NOT-APPLIED: ", log)
        self.assertIn('"code": "not-the-wrapper"', log)
        self.assertIn(f'"effective_git": "{plain / "git"}"', log)
        self.assertIn('"effective_version": "git version 2.52.0"', log)
        substitution = self.record_git_substitution()
        self.assertEqual(False, substitution["applied"])
        self.assertEqual("not-the-wrapper", substitution["code"])
        self.assertIsNone(substitution["directory"])
        self.assertEqual(str(plain / "git"), substitution["effective_git"])

    def test_directory_failure_launches_with_path_unchanged(self) -> None:
        local, _real, _plain = self.fake_host_gits()
        path = f"{local}:/usr/bin:/bin"
        environment = {**self.environment, "PATH": path}

        def under_a_file(runtime_root: Path) -> Path:
            blocker = runtime_root / "blocker"
            blocker.write_text("a file, not a directory\n", encoding="utf-8")
            return blocker / "unwrapped-git"

        with mock.patch.object(
            start_unit, "_unwrapped_git_directory", side_effect=under_a_file
        ):
            rc, _output, error = self.invoke(["--", "full"], environment=environment)

        self.assertEqual(0, rc, error)
        self.assertEqual(path, self.unit_path_value(self.launched_systemd_command()))
        log = (self.root / "run.log").read_text(encoding="utf-8")
        self.assertIn("validate-run: git substitution NOT-APPLIED: ", log)
        self.assertIn('"code": "materialize-failed"', log)
        substitution = self.record_git_substitution()
        self.assertEqual("materialize-failed", substitution["code"])
        self.assertIsNone(substitution["directory"])
        self.assertEqual(str(local / "git"), substitution["effective_git"])

    def test_log_collision_refusal_leaves_no_unwrapped_git_directory(self) -> None:
        # The substitution directory lives in the runtime root, not under
        # ignored/validate/artifacts/<unit>, whose absence
        # correct_owner_watch_handles.py requires of a refused launch.
        local, _real, _plain = self.fake_host_gits()
        environment = {**self.environment, "PATH": f"{local}:/usr/bin:/bin"}
        log = self.root / "run.log"
        log.write_text("launcher-owned collision\n")

        rc, _output, error = self.invoke(["--", "full"], environment=environment)

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn(f"validation log already exists: {log}", error)
        self.assertEqual("launcher-owned collision\n", log.read_text())
        artifacts = self.root / "ignored/validate/artifacts/validate-test"
        self.assertFalse(artifacts.exists() or artifacts.is_symlink(), artifacts)
        self.assertFalse(self.unwrapped_git_directory().exists())
        self.assertFalse(self.runtime_roots[-1].exists())
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("refused", record["state"])
        self.assertEqual("applied", record["git_substitution"]["code"])

    def test_reverie_launch_leaves_path_and_git_alone(self) -> None:
        local, _real, _plain = self.fake_host_gits()
        path = f"{local}:/usr/bin:/bin"
        environment = {**self.environment, "PATH": path}
        materialize_hermit_shape = self.fake._materialize_fresh_checkout

        def add_validate_sh(checkout: Path) -> None:
            validate_sh = checkout / "validate.sh"
            validate_sh.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            validate_sh.chmod(0o755)

        def materialize_reverie_shape() -> None:
            materialize_hermit_shape()
            assert self.fake.fresh is not None
            add_validate_sh(self.fake.fresh)

        add_validate_sh(self.checkout)

        self.fake._materialize_fresh_checkout = materialize_reverie_shape  # type: ignore[method-assign]
        # The managed source repository a Reverie launch looks for beneath the
        # state root; this fixture's checkout stands in for it.
        (self.root / "reverie").symlink_to(self.checkout)
        # Reverie's current-main admission and count-producer hashes are not
        # what this test is about: bypass the one, hash the real tool's files.
        hash_sources = start_unit.reverie_node.count_source_hashes
        tool_root = Path(start_unit.__file__).resolve().parents[2]
        with mock.patch.object(start_unit, "preflight", return_value=None), \
                mock.patch.object(
                    start_unit.reverie_node,
                    "count_source_hashes",
                    side_effect=lambda _root: hash_sources(tool_root),
                ):
            _rc, _output, error = self.invoke(
                ["--repo", "rrnewton/reverie"], environment=environment
            )

        # This fixture does not model a Reverie verdict (no count sidecar, no
        # ledger row), so the exit code is not asserted; the unit launched.
        systemd = [
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        ]
        self.assertEqual(1, len(systemd), error)
        self.assertEqual(path, self.unit_path_value(systemd[0]))
        self.assertFalse(self.unwrapped_git_directory().exists())
        self.assertNotIn("git substitution", (self.root / "run.log").read_text())
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("rrnewton/reverie", record["repo"])
        self.assertNotIn("git_substitution", record)

    def test_dry_run_plans_unwrapped_git_without_creating_it(self) -> None:
        local, _real, _plain = self.fake_host_gits()
        environment = {**self.environment, "PATH": f"{local}:/usr/bin:/bin"}
        created: list[Path] = []
        original_materialize = start_unit.unwrapped_git.materialize

        def forbid(substitution: object) -> object:
            created.append(Path(str(getattr(substitution, "directory"))))
            return original_materialize(substitution)

        with mock.patch.object(start_unit.unwrapped_git, "materialize", side_effect=forbid):
            rc, output, error = self.invoke(
                ["--dry-run", "--", "full"], environment=environment
            )

        self.assertEqual(0, rc, error)
        self.assertEqual([], created)
        command_lines = [
            line.removeprefix("COMMAND ")
            for line in output.splitlines()
            if line.startswith("COMMAND ")
        ]
        self.assertEqual(1, len(command_lines), output)
        directory = self.unwrapped_git_directory()
        self.assertEqual(
            f"{directory}:{local}:/usr/bin:/bin",
            self.unit_path_value(shlex.split(command_lines[0])),
        )
        self.assertFalse(directory.exists())

    def test_malformed_cpu_placement_override_refuses_before_launch(self) -> None:
        environment = {**self.environment, "DEV_HERMIT_EXCLUDE_CPUS": "0-x"}
        rc, _output, error = self.invoke(["--", "full"], environment=environment)

        self.assertEqual(2, rc, error)
        self.assertIn(
            "validate-run: REFUSED: DEV_HERMIT_EXCLUDE_CPUS='0-x' is not a CPU list",
            error,
        )
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            )
        )

    def test_materialized_profile_artifact_survives_checkout_cleanup(self) -> None:
        profile = (
            self.root / "ignored/validate/artifacts/validate-test/dagrun-profiles/"
            "step_profiles-fixture.csv"
        )
        contents = b"step,ok,oom_kills,memory_events_oom_kill\nprobe,true,2,2\n"

        def run(command: list[str], **kwargs: object):
            if command[0] == "systemd-run" and "validate-lock" in command:
                values = [item for item in command if item.startswith("RUN_NODE_PERF_DIR=")]
                self.assertEqual([f"RUN_NODE_PERF_DIR={profile.parent}"], values)
                # Model the driver's existing profile writer, including raw OOM
                # evidence that contradicts a passing StepOutcome.
                profile.parent.mkdir(parents=True, exist_ok=True)
                profile.write_bytes(contents)
            return self.fake(command, **kwargs)

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"], run=run)

        self.assertEqual(0, rc, error)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertFalse(self.fake.fresh.exists())
        self.assertEqual(contents, profile.read_bytes())

    def test_entry_authority_ignores_completed_disjoint_checkout(self) -> None:
        stale, _record = self.managed_cleanup_record("entry-stale")

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(stale.exists())
        self.assertIn("validate-run: ENTRY-AUTHORITY state=ready ", error)
        self.assertIn("run-handles=1", error)
        classify_index = next(
            index
            for index, command in enumerate(self.fake.commands)
            if wrkslots_action(command, "classify-create-journals")
        )
        create_index = next(
            index
            for index, command in enumerate(self.fake.commands)
            if wrkslots_action(command, "create")
        )
        self.assertLess(classify_index, create_index)
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )

    def assert_entry_classifies_in_place_cargo_home_without_mutation(
        self,
        *,
        suffix: str,
        temporary: object,
        include_temporary: bool = True,
        current: bool,
        journal_state: str = "dead-incomplete-create",
    ) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            suffix,
            temporary=temporary,
            include_temporary=include_temporary,
            current=current,
        )
        journal = self.root / "worktrees/ACTIVE.devbig014.journal"
        journal.parent.mkdir(parents=True, exist_ok=True)
        journal.write_bytes(b'{"kind":"create","slot":"series-rows"}\n')

        protected = (
            journal,
            checkout / "preserved",
            cargo_home / "preserved",
            record_path,
        )
        before = {
            path: (
                path.read_bytes(),
                path.stat().st_ino,
                path.stat().st_size,
                path.stat().st_mtime_ns,
            )
            for path in protected
        }
        prospective_slot = (
            f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        )
        self.fake.create_journal_classification_output = (
            self.create_journal_report(
                prospective_slot=prospective_slot,
                state=journal_state,
                observed_head=("e" * 40 if journal_state.startswith("live-") else SHA),
            )
        )

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=1", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        self.assertTrue(
            any(
                wrkslots_action(command, "classify-create-journals")
                for command in self.fake.commands
            )
        )
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in self.fake.commands
                for action in (
                    "recover",
                    "recover-ownerless-validate-batch",
                    "remove-validate-batch",
                )
            )
        )
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())
        after = {
            path: (
                path.read_bytes(),
                path.stat().st_ino,
                path.stat().st_size,
                path.stat().st_mtime_ns,
            )
            for path in protected
        }
        self.assertEqual(before, after)

    def test_create_journal_scope_parser_accepts_real_pinned_provider_output(
        self,
    ) -> None:
        provider_tests = (
            start_unit.ROOT
            / "hermit/agent-utils/py/wrkslots/tests/test_lifecycle.py"
        )
        self.assertTrue(provider_tests.is_file())
        original_sys_path = list(sys.path)
        try:
            helpers = runpy.run_path(str(provider_tests))
        finally:
            sys.path[:] = original_sys_path

        fixture_root = self.root / "provider-fixture"
        fixture_root.mkdir()
        project, _repository, _remote = helpers["make_project"](
            fixture_root,
            worktrees_directory="worktrees/slots",
            layout="flat",
        )
        interrupted = helpers["create"](
            project,
            slot="series-rows",
            agent="series-rows",
            branch="series-rows-parent-20260914",
            repository_name="repo",
            checkout_name="parent",
            slot_type="agent",
            env={"WRKSLOTS_TEST_INTERRUPT": "after-create-worktree"},
        )
        self.assertEqual(86, interrupted.returncode, interrupted.stderr)
        checkout = helpers["checkout"](
            project,
            slot="series-rows",
            name="parent",
            slot_type="agent",
        )
        (checkout / "unfinished.txt").write_text("authored work\n")
        journal = helpers["create_journal_path"](project, "series-rows")
        before = (
            journal.read_bytes(),
            journal.stat(follow_symlinks=False).st_ino,
            journal.stat(follow_symlinks=False).st_mtime_ns,
        )
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"

        scopes = start_unit.classify_create_journal_scopes(
            project,
            prospective_slot=prospective_slot,
            prospective_checkout=(
                project / "worktrees/validate" / prospective_slot
            ),
            run=start_unit.run_command,
            tool_root=start_unit.ROOT,
        )

        self.assertEqual(1, len(scopes))
        scope = scopes[0]
        self.assertEqual("live-incomplete-create", scope.state)
        self.assertEqual("series-rows", scope.slot)
        self.assertEqual("series-rows", scope.agent)
        self.assertEqual(
            (project / "worktrees/slots/series-rows").resolve(), scope.slot_path
        )
        self.assertEqual({"parent"}, scope.dirty_checkouts)
        self.assertEqual(
            ("parent", "series-rows-parent-20260914"),
            (scope.checkouts[0].name, scope.checkouts[0].branch),
        )
        self.assertEqual(
            helpers["git"](checkout, "rev-parse", "HEAD").stdout.strip(),
            scope.checkouts[0].head,
        )
        after = (
            journal.read_bytes(),
            journal.stat(follow_symlinks=False).st_ino,
            journal.stat(follow_symlinks=False).st_mtime_ns,
        )
        self.assertEqual(before, after)

    def test_entry_classifies_current_in_place_cargo_home_without_mutation(self) -> None:
        self.assert_entry_classifies_in_place_cargo_home_without_mutation(
            suffix="current-in-place", temporary=False, current=True
        )

    def test_entry_classifies_historical_in_place_cargo_home_without_mutation(
        self,
    ) -> None:
        self.assert_entry_classifies_in_place_cargo_home_without_mutation(
            suffix="historical-in-place",
            temporary=False,
            include_temporary=False,
            current=False,
            journal_state="live-incomplete-create",
        )

    def assert_entry_classifies_unknown_record_in_verified_journal_slot(
        self, *, admitted: bool
    ) -> None:
        suffix = "unknown-admitted" if admitted else "unknown-before-admission"
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            suffix,
            temporary=False,
            current=True,
            state="unknown",
            admitted=admitted,
        )
        self.fake.collected_unit = True
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        self.fake.create_journal_classification_output = (
            self.create_journal_report(prospective_slot=prospective_slot)
        )
        protected = (checkout / "preserved", cargo_home / "preserved", record_path)
        before = {
            path: (
                path.read_bytes(),
                path.stat(follow_symlinks=False).st_ino,
                path.stat(follow_symlinks=False).st_mtime_ns,
            )
            for path in protected
        }

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=1", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())
        self.assertEqual(
            before,
            {
                path: (
                    path.read_bytes(),
                    path.stat(follow_symlinks=False).st_ino,
                    path.stat(follow_symlinks=False).st_mtime_ns,
                )
                for path in protected
            },
        )
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_classifies_unknown_admitted_record_in_verified_journal_slot(
        self,
    ) -> None:
        self.assert_entry_classifies_unknown_record_in_verified_journal_slot(
            admitted=True
        )

    def test_entry_classifies_unknown_pre_admission_record_in_verified_journal_slot(
        self,
    ) -> None:
        self.assert_entry_classifies_unknown_record_in_verified_journal_slot(
            admitted=False
        )

    def test_entry_ignores_disjoint_current_record_missing_temporary_checkout(
        self,
    ) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "current-missing-temporary",
            include_temporary=False,
            current=True,
        )
        before = (record_path.read_bytes(), cargo_home.stat().st_ino)

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in self.fake.commands
                for action in ("recover", "recover-ownerless-validate-batch")
            )
        )
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())
        self.assertEqual(before, (record_path.read_bytes(), cargo_home.stat().st_ino))

    def test_entry_ignores_disjoint_unidentified_historical_record(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "historical-missing-producer",
            include_temporary=False,
            current=False,
        )
        record = json.loads(record_path.read_text())
        record.pop("producer")
        record_path.write_text(json.dumps(record, sort_keys=True) + "\n")
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        self.fake.create_journal_classification_output = (
            self.create_journal_report(prospective_slot=prospective_slot)
        )

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_ignores_disjoint_malformed_temporary_checkout(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "malformed-in-place", temporary=None, current=True
        )
        before = record_path.read_bytes()

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in self.fake.commands
                for action in ("recover", "recover-ownerless-validate-batch")
            )
        )
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())
        self.assertEqual(before, record_path.read_bytes())

    def test_entry_retains_uncertain_current_in_place_record_without_journal(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "uncertain-in-place", temporary=False, current=True, state="unknown"
        )
        self.fake.collected_unit = True
        before = record_path.read_bytes()

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ):
            rc, _output, error = self.invoke(["--dry-run"])

        # Non-disposable storage does not become disposable or acquire a known
        # outcome merely because its creation journal has been retired.
        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=1", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in self.fake.commands
                for action in ("recover", "recover-ownerless-validate-batch")
            )
        )
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())
        self.assertEqual(before, record_path.read_bytes())

    def test_entry_ignores_disjoint_legacy_record_without_journal(self) -> None:
        _checkout, _cargo_home, record_path = self.in_place_cleanup_record(
            "legacy-without-journal", include_temporary=False, current=False
        )
        before = record_path.read_bytes()
        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertEqual(before, record_path.read_bytes())

    def test_retained_in_place_storage_preserves_live_and_uncertain_observations(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "retained-observation", temporary=False, current=True, state="unknown"
        )
        record = start_unit.run_registry.read_record(record_path)
        prospective_slot = "validate-fresh-retained-observation"
        storage_reason = (
            "non-disposable in-place storage remains untouched; "
            "outcome and process state are unchanged"
        )
        for unit_state, expected_reason in (
            ("running", "unit-running"),
            ("absent", "unit state unavailable and run record is not terminal"),
            ("query-error", "retained unit query unavailable"),
        ):
            with self.subTest(unit_state=unit_state):
                self.fake.collected_unit = unit_state == "absent"

                def run(command: list[str], **kwargs: object):
                    if command[:3] == ["systemctl", "--user", "show"]:
                        if unit_state == "running":
                            return completed(command, stdout=(
                                "LoadState=loaded\nActiveState=active\nSubState=running\n"
                                "InvocationID=retained-live-unit-fixture\n"
                            ))
                        if unit_state == "query-error":
                            raise OSError(expected_reason)
                    return self.fake(command, **kwargs)

                before = self.filesystem_snapshot(self.root)
                observation = start_unit.terminal_cleanup_unit(
                    self.root, record_path, record, run=run
                )
                self.assertTrue(observation.blocks_entry)
                self.assertEqual(expected_reason, observation.reason)
                report = start_unit.sweep_completed_checkouts(
                    self.root,
                    run=run,
                    tool_root=self.root,
                    classify_ownerless_only=True,
                    prospective_slot=prospective_slot,
                    prospective_checkout=self.root / "worktrees/validate" / prospective_slot,
                )

                self.assertEqual([], report["removed"])
                self.assertEqual([], report["removed_cargo_homes"])
                self.assertEqual([], report["scorecard_handoffs"])
                self.assertEqual([{
                    "blocks_entry": False,
                    "checkout": str(checkout),
                    **({"cleanup_failed": True} if unit_state == "query-error" else {}),
                    "reason": f"{expected_reason}; {storage_reason}",
                }], report["retained"])
                self.assertEqual(before, self.filesystem_snapshot(self.root))
                self.assertEqual(record, start_unit.run_registry.read_record(record_path))
                self.assertTrue(checkout.is_dir())
                self.assertTrue(cargo_home.is_dir())

    def entry_classification(self) -> dict[str, Any]:
        prospective_slot = "validate-fresh-next-run"
        return start_unit.sweep_completed_checkouts(
            self.root,
            run=self.fake,
            tool_root=self.root,
            classify_ownerless_only=True,
            prospective_slot=prospective_slot,
            prospective_checkout=self.root / "worktrees/validate" / prospective_slot,
        )

    def test_entry_blocker_names_the_artifact_that_still_exists(self) -> None:
        """The blocking row must name the path that is actually retained.

        Measured on devbig014 2026-09-18, and this cost an hour of
        misdirection. Run record
        validate-main-dev-hermit-f615104d0df2-20260918T035200Z.json (RUN 1861,
        state=completed) blocked full-profile admission for eighteen
        consecutive launch attempts. The refusal named

            1 retained checkout(s) ... first retained:
            worktrees/validate/validate-fresh-f615104d0df2-3692792-7455f186

        and THAT DIRECTORY DID NOT EXIST. The row was discovered only because
        `recorded_cleanup_path_exists` was still true for its CARGO HOME, which
        was removed at 09:16:28Z -- at which point the block cleared on its own,
        with the run record byte-identical and the checkout still absent.

        So the message pointed at an already-deleted checkout while the artifact
        genuinely holding entry was the cargo home, which the message never
        named. An operator following it looks for a directory that is not there.
        """
        fresh, record_path = self.managed_cleanup_record("misnamed-blocker")
        cargo_home = self.root / "ignored/validate/cargo-homes/validate-cargo-misnamed"
        cargo_home.mkdir(parents=True, exist_ok=True)
        record = start_unit.run_registry.read_record(record_path)
        record["cargo_home"] = str(cargo_home)
        start_unit.run_registry.write_record(record_path, record)
        shutil.rmtree(fresh)
        self.assertFalse(fresh.exists())
        self.assertTrue(cargo_home.is_dir())

        report = self.entry_classification()

        blocking = [item for item in report["retained"] if item.get("blocks_entry")]
        self.assertEqual(1, len(blocking), report["retained"])
        named = blocking[0]["checkout"]
        self.assertTrue(
            Path(named).exists(),
            f"entry blocker named {named!r}, which does not exist; the retained "
            f"artifact is {str(cargo_home)!r}. Blocking row: {blocking[0]!r}",
        )

    def test_entry_blocker_names_a_present_checkout_unchanged(self) -> None:
        """Control: when the checkout IS present it must still be the named path.

        Without this, 'name something that exists' could be satisfied by
        renaming every blocker to the cargo home, and the suite would stay
        green while the ordinary message got worse.
        """
        fresh, _record_path = self.managed_cleanup_record("present-blocker")
        self.assertTrue(fresh.is_dir())

        report = self.entry_classification()

        self.assertEqual(1, len(report["retained"]), report["retained"])
        self.assertEqual(str(fresh), report["retained"][0]["checkout"])
        self.assertTrue(Path(report["retained"][0]["checkout"]).exists())
        self.assertTrue(fresh.is_dir())

    def test_entry_refuses_indeterminate_create_journal_classification(self) -> None:
        checkout, cargo_home, _record_path = self.in_place_cleanup_record(
            "indeterminate-journal", temporary=False, current=True, state="unknown"
        )
        self.fake.collected_unit = True
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        report = json.loads(
            self.create_journal_report(prospective_slot=prospective_slot)
        )
        report["journals"][0]["state"] = "indeterminate-incomplete-create"
        report["journals"][0]["owner_state"] = "indeterminate"
        report["journals"][0]["coordinator_state"] = "indeterminate"
        self.fake.create_journal_classification_output = json.dumps(report)

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("uncertain process or registry state", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_refuses_when_classified_journal_disappears(self) -> None:
        checkout, cargo_home, _record_path = self.in_place_cleanup_record(
            "journal-disappeared", temporary=False, current=True, state="unknown"
        )
        self.fake.collected_unit = True
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        report = self.create_journal_report(prospective_slot=prospective_slot)
        journal = self.root / "worktrees/ACTIVE.devbig014.journal"
        journal.unlink()
        self.fake.create_journal_classification_output = report

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("invalid create-journal classification", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_ignores_disjoint_record_with_different_source_checkout(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "source-mismatch", temporary=False, current=True, state="unknown"
        )
        record = start_unit.run_registry.read_record(record_path)
        record["source_checkout"] = str(self.checkout)
        start_unit.run_registry.write_record(record_path, record)
        self.fake.collected_unit = True
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        self.fake.create_journal_classification_output = (
            self.create_journal_report(prospective_slot=prospective_slot)
        )

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_refuses_provider_checkout_path_disagreement(self) -> None:
        checkout, cargo_home, _record_path = self.in_place_cleanup_record(
            "provider-path-mismatch", temporary=False, current=True, state="unknown"
        )
        self.fake.collected_unit = True
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        report = json.loads(
            self.create_journal_report(prospective_slot=prospective_slot)
        )
        report["journals"][0]["checkouts"][0]["path"] = (
            "worktrees/slots/somewhere-else"
        )
        self.fake.create_journal_classification_output = json.dumps(report)

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("checkout is outside its slot", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_ignores_disjoint_malformed_record_inside_classified_slot(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "malformed-current-scope",
            temporary=False,
            current=True,
            state="unknown",
        )
        record = json.loads(record_path.read_text())
        record.pop("e2e_result_root")
        record_path.write_text(json.dumps(record, sort_keys=True) + "\n")
        self.fake.collected_unit = True
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        self.fake.create_journal_classification_output = (
            self.create_journal_report(prospective_slot=prospective_slot)
        )

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_ignores_disjoint_legacy_record_with_different_agent(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "agent-mismatch", include_temporary=False, current=False, state="unknown"
        )
        record = start_unit.run_registry.read_record(record_path)
        record["agent"] = "different-agent"
        start_unit.run_registry.write_record(record_path, record)
        self.fake.collected_unit = True
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        self.fake.create_journal_classification_output = (
            self.create_journal_report(prospective_slot=prospective_slot)
        )

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_classification_is_read_only_for_the_complete_tree(self) -> None:
        fresh, record_path = self.managed_cleanup_record("read-only-complete-tree")
        prospective_slot = "validate-fresh-aaaaaaaaaaaa-read-only"
        before = self.filesystem_snapshot(self.root)

        def forbidden(*_args: object, **_kwargs: object) -> object:
            raise AssertionError("read-only entry classification attempted a write")

        with mock.patch.object(
            start_unit, "publish_recorded_scorecard_handoff", side_effect=forbidden
        ), mock.patch.object(
            start_unit, "archive_orphaned_receipts", side_effect=forbidden
        ), mock.patch.object(
            start_unit, "remove_fresh_checkouts_batch", side_effect=forbidden
        ), mock.patch.object(
            start_unit.run_registry, "update_record", side_effect=forbidden
        ):
            start_unit.require_validate_entry_cleanup(
                self.root,
                prospective_slot=prospective_slot,
                prospective_checkout=(
                    self.root / "worktrees/validate" / prospective_slot
                ),
                run=self.fake,
                tool_root=self.root,
            )

        self.assertEqual(before, self.filesystem_snapshot(self.root))
        self.assertTrue(fresh.is_dir())
        self.assertTrue(record_path.is_file())
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in self.fake.commands
                for action in (
                    "recover",
                    "recover-ownerless-validate-batch",
                    "remove-validate-batch",
                )
            )
        )

    def test_entry_classifies_all_series_rows_without_changing_the_tree(self) -> None:
        records = [
            self.in_place_cleanup_record(
                "series-terminal", temporary=False, current=True, state="completed"
            ),
            self.in_place_cleanup_record(
                "series-unknown-admitted",
                temporary=False,
                current=True,
                state="unknown",
                admitted=True,
            ),
            self.in_place_cleanup_record(
                "series-unknown-before-admission",
                temporary=False,
                current=True,
                state="unknown",
                admitted=False,
            ),
            self.in_place_cleanup_record(
                "series-running", temporary=False, current=True, state="running"
            ),
        ]
        for index, (_checkout, _cargo_home, record_path) in enumerate(records):
            record = start_unit.run_registry.read_record(record_path)
            record["process_identity"] = {
                "pid": 12345 + index,
                "start_ticks": 67890 + index,
                "boot_id": "retained-historical-boot",
            }
            if index == 0:
                record.update(result="failure", exit_code=1, final_validate_status="FAILED")
            elif record["state"] == "running":
                record.pop("finished_at")
                record.pop("exit_code")
                record.pop("final_validate_status")
            start_unit.run_registry.write_record(record_path, record)
        prospective_slot = "validate-fresh-aaaaaaaaaaaa-series-proof"
        report = json.loads(self.create_journal_report(prospective_slot=prospective_slot))
        original_record_bytes = [path.read_bytes() for _, _, path in records]

        def forbidden(*_args: object, **_kwargs: object) -> object:
            raise AssertionError("read-only entry classification attempted a write")

        for stage in ("original-journal", "new-slot-owner", "retired-journal", "removed-cargo"):
            with self.subTest(stage=stage):
                if stage == "new-slot-owner":
                    # A different current owner/HEAD does not rewrite historical
                    # evidence or authorize access to that owner's source.
                    report = json.loads(self.create_journal_report(
                        prospective_slot=prospective_slot,
                        state="live-incomplete-create",
                        observed_head="e" * 40,
                    ))
                    report["journals"][0]["agent"] = "new-series-owner"
                elif stage == "retired-journal":
                    (self.root / "worktrees/ACTIVE.devbig014.journal").unlink()
                    report["journals"] = []
                elif stage == "removed-cargo":
                    for _checkout, cargo_home, _record_path in records:
                        shutil.rmtree(cargo_home)
                self.fake.create_journal_classification_output = json.dumps(report)
                for unit_state in ("terminal", "running", "absent"):
                    with self.subTest(unit_state=unit_state):
                        self.fake.collected_unit = unit_state == "absent"

                        def run(command: list[str], **kwargs: object):
                            if unit_state == "running" and command[:3] == ["systemctl", "--user", "show"]:
                                return completed(command, stdout=(
                                    "LoadState=loaded\nActiveState=active\nSubState=running\n"
                                    "InvocationID=current-live-unit-fixture\n"
                                ))
                            return self.fake(command, **kwargs)

                        before = self.filesystem_snapshot(self.root)
                        error = io.StringIO()
                        with mock.patch.object(
                            start_unit, "publish_recorded_scorecard_handoff", side_effect=forbidden
                        ), mock.patch.object(
                            start_unit, "archive_orphaned_receipts", side_effect=forbidden
                        ), mock.patch.object(
                            start_unit, "remove_fresh_checkouts_batch", side_effect=forbidden
                        ), mock.patch.object(
                            start_unit.run_registry, "update_record", side_effect=forbidden
                        ), contextlib.redirect_stderr(error):
                            start_unit.require_validate_entry_cleanup(
                                self.root,
                                prospective_slot=prospective_slot,
                                prospective_checkout=(
                                    self.root / "worktrees/validate" / prospective_slot
                                ),
                                run=run,
                                tool_root=self.root,
                            )
                        self.assertIn("ENTRY-AUTHORITY state=ready", error.getvalue())
                        self.assertIn("run-handles=4", error.getvalue())
                        self.assertEqual(before, self.filesystem_snapshot(self.root))
                        self.assertEqual(
                            original_record_bytes,
                            [path.read_bytes() for _, _, path in records],
                        )
                        for _, _, record_path in records:
                            reason, refusal = start_unit.classify_retained_current_in_place_record(
                                self.root, start_unit.run_registry.read_record(record_path)
                            )
                            self.assertIsNone(refusal)
                            self.assertEqual(
                                "non-disposable in-place storage remains untouched; "
                                "outcome and process state are unchanged",
                                reason,
                            )
        for checkout, cargo_home, record_path in records:
            self.assertTrue(checkout.is_dir())
            self.assertFalse(cargo_home.exists())
            self.assertTrue(record_path.is_file())

    def test_entry_ignores_only_proven_in_place_shared_cargo_storage(self) -> None:
        checkout, private_cargo, record_path = self.in_place_cleanup_record(
            "shared-cargo-control", temporary=False, current=True, state="unknown"
        )
        shared_cargo = self.root / "modeled-user-home/.cargo"
        shared_cargo.mkdir(parents=True)
        (shared_cargo / "preserved").write_text("shared cache\n")
        record = start_unit.run_registry.read_record(record_path)
        record["cargo_home"] = str(shared_cargo)
        start_unit.run_registry.write_record(record_path, record)
        shutil.rmtree(private_cargo)
        before = self.filesystem_snapshot(self.root)

        with mock.patch.object(
            start_unit, "process_user_cargo_home", return_value=shared_cargo.resolve()
        ):
            reason, refusal = start_unit.classify_retained_current_in_place_record(
                self.root, record
            )
            groups, singles, rows, retained, discovery = (
                start_unit.discover_cleanup_groups(
                    self.root, self.root / "ignored/validate/runs"
                )
            )
            start_unit.require_validate_entry_cleanup(
                self.root,
                prospective_slot="validate-fresh-shared-cargo-control",
                prospective_checkout=(
                    self.root
                    / "worktrees/validate/validate-fresh-shared-cargo-control"
                ),
                run=self.fake,
                tool_root=self.root,
            )
            sweep = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual(
            "non-disposable in-place storage remains untouched; "
            "outcome and process state are unchanged",
            reason,
        )
        self.assertIsNone(refusal)
        self.assertEqual({}, groups)
        self.assertEqual([], singles)
        self.assertEqual([], rows)
        self.assertEqual([], retained)
        self.assertEqual(1, discovery["run_handle_candidates"])
        self.assertEqual([], sweep["removed"])
        self.assertEqual([], sweep["removed_cargo_homes"])
        self.assertEqual([], sweep["retained"])
        self.assertEqual(before, self.filesystem_snapshot(self.root))
        self.assertEqual(record, start_unit.run_registry.read_record(record_path))
        self.assertTrue(checkout.is_dir())
        self.assertTrue(shared_cargo.is_dir())
        self.assertFalse(any(
            wrkslots_action(command, action)
            for command in self.fake.commands
            for action in (
                "recover",
                "recover-ownerless-validate-batch",
                "remove-validate-batch",
            )
        ))

        controls = (
            (
                {
                    "temporary_checkout": True,
                    "checkout": str(self.root / "absent-temporary-checkout"),
                    "source_checkout": str(self.root / "absent-temporary-checkout"),
                },
                "temporary",
            ),
            ({"producer": start_unit.LEGACY_IN_PLACE_PRODUCER}, "legacy"),
            ({"source_checkout": str(checkout / "other")}, "source-mismatch"),
            ({"cargo_home": str(self.root / "other-cargo")}, "other-cargo"),
            ({"schema_version": 999}, "schema"),
            (
                {
                    "checkout": str(shared_cargo),
                    "source_checkout": str(shared_cargo),
                },
                "overlap-shared",
            ),
            (
                {
                    "checkout": str(shared_cargo / "source"),
                    "source_checkout": str(shared_cargo / "source"),
                },
                "overlap-shared-child",
            ),
        )
        for changes, name in controls:
            with self.subTest(name=name):
                candidate = {**record, **changes}
                cargo = Path(str(candidate["cargo_home"]))
                cargo.mkdir(parents=True, exist_ok=True)
                with mock.patch.object(
                    start_unit,
                    "process_user_cargo_home",
                    return_value=shared_cargo.resolve(),
                ):
                    self.assertTrue(
                        start_unit.recorded_cleanup_path_exists(
                            self.root, candidate
                        )
                    )

        shared_alias = self.root / "modeled-user-home/cargo-alias"
        shared_alias.symlink_to(shared_cargo, target_is_directory=True)
        with mock.patch.object(
            start_unit, "process_user_cargo_home", return_value=shared_cargo.resolve()
        ):
            self.assertTrue(
                start_unit.recorded_cleanup_path_exists(
                    self.root, {**record, "cargo_home": str(shared_alias)}
                )
            )

        missing_shared = self.root / "modeled-user-home/missing-cargo"
        with mock.patch.object(
            start_unit, "process_user_cargo_home", return_value=missing_shared.resolve()
        ):
            self.assertFalse(
                start_unit.recorded_cleanup_path_exists(
                    self.root, {**record, "cargo_home": str(missing_shared)}
                )
            )

        with mock.patch.object(
            start_unit.pwd, "getpwuid", side_effect=KeyError("unmapped uid")
        ):
            self.assertIsNone(start_unit.process_user_cargo_home())
            self.assertTrue(
                start_unit.recorded_cleanup_path_exists(self.root, record)
            )
        for invalid_home in ("", "."):
            with self.subTest(invalid_home=invalid_home), mock.patch.object(
                start_unit.pwd,
                "getpwuid",
                return_value=mock.Mock(pw_dir=invalid_home),
            ):
                self.assertIsNone(start_unit.process_user_cargo_home())
                self.assertTrue(
                    start_unit.recorded_cleanup_path_exists(self.root, record)
                )

    def test_entry_ignores_ambiguous_storage_when_every_path_is_disjoint(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "ambiguous-in-place", temporary=False, current=True, state="unknown"
        )
        original = start_unit.run_registry.read_record(record_path)
        managed = self.root / "worktrees/validate/validate-fresh-not-in-place"
        absent_cargo = cargo_home.with_name("validate-cargo-already-removed")
        controls = (
            ({"checkout": str(managed), "source_checkout": str(managed)}, "temporary_checkout must be true"),
            ({"temporary_checkout": 0}, "temporary_checkout must be a boolean"),
            ({"cargo_home": str(self.root / "foreign-cargo")}, "Cargo home is outside"),
            ({"checkout": str(cargo_home), "source_checkout": str(cargo_home)}, "Cargo home overlaps"),
            ({"checkout": str(cargo_home / "source"), "source_checkout": str(cargo_home / "source")}, "Cargo home overlaps"),
            ({"checkout": str(absent_cargo), "source_checkout": str(absent_cargo), "cargo_home": str(absent_cargo)}, "Cargo home overlaps"),
            ({"schema_version": 999}, "malformed|unsupported schema"),
            ({"process_identity": {"pid": True, "start_ticks": 1, "boot_id": "boot"}}, "malformed"),
        )
        self.fake.collected_unit = True
        for changes, expected in controls:
            with self.subTest(changes=changes):
                record_path.write_text(json.dumps({**original, **changes}) + "\n")
                before = self.filesystem_snapshot(self.root)
                record = {**original, **changes}
                classified, refusal = start_unit.classify_recorded_checkout(self.root, record)
                self.assertIsNone(classified)
                if refusal is None:
                    reason, refusal = start_unit.classify_retained_current_in_place_record(
                        self.root, record
                    )
                    self.assertIsNone(reason)
                self.assertRegex(refusal or "", expected)
                # Cleanup still classifies these malformed rows strictly, but
                # launch admission is scoped only to the prospective path.
                if start_unit.recorded_cleanup_path_exists(self.root, record):
                    start_unit.require_validate_entry_cleanup(
                        self.root,
                        prospective_slot="validate-fresh-ambiguity-control",
                        prospective_checkout=self.root / "worktrees/validate/validate-fresh-ambiguity-control",
                        run=self.fake,
                        tool_root=self.root,
                    )
                self.assertEqual(before, self.filesystem_snapshot(self.root))
        self.assertFalse(any(
            wrkslots_action(command, action)
            for command in self.fake.commands
            for action in ("create", "recover", "recover-ownerless-validate-batch", "remove-validate-batch")
        ))
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_refuses_malformed_storage_path_authority(self) -> None:
        _checkout, _cargo_home, record_path = self.in_place_cleanup_record(
            "malformed-path-authority", temporary=False, current=True
        )
        original = start_unit.run_registry.read_record(record_path)
        prospective_slot = "validate-fresh-malformed-path-authority"
        prospective = self.root / "worktrees/validate" / prospective_slot
        self.fake.create_journal_classification_output = (
            self.create_journal_report(prospective_slot=prospective_slot)
        )

        for field, value in (
            ("checkout", "relative"),
            ("cargo_home", None),
            ("cargo_home", os.path.relpath(self.root / "outside")),
        ):
            with self.subTest(field=field, value=value):
                record_path.write_text(json.dumps({**original, field: value}) + "\n")
                with self.assertRaisesRegex(
                    start_unit.ValidateEntryCleanupRefused,
                    f"invalid {field} path",
                ):
                    start_unit.require_validate_entry_cleanup(
                        self.root,
                        prospective_slot=prospective_slot,
                        prospective_checkout=prospective,
                        run=self.fake,
                        tool_root=self.root,
                    )

    def test_explicit_sweep_still_recovers_terminal_in_place_cargo_home(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "explicit-sweep-in-place", temporary=False, current=True
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual([str(cargo_home)], report["removed_cargo_homes"])
        self.assertEqual([], report["retained"])
        self.assertTrue(checkout.is_dir())
        self.assertFalse(cargo_home.exists())
        self.assertIsInstance(
            start_unit.run_registry.read_record(record_path).get(
                "cargo_home_removed_at"
            ),
            str,
        )
        self.assertTrue(
            any(
                "--ownerless-validate-cargo-home" in command
                for command in self.fake.commands
            )
        )

    def test_sweep_bounds_terminal_cargo_homes_to_one_per_pass(self) -> None:
        inputs = [
            self.in_place_cleanup_record(
                f"cargo-batch-{index}", temporary=False, current=True
            )
            for index in range(3)
        ]

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        cargo_homes = [cargo_home for _checkout, cargo_home, _record in inputs]
        self.assertEqual([str(cargo_homes[0])], report["removed_cargo_homes"])
        self.assertEqual(2, len(report["deferred"]))
        commands = [
            command
            for command in self.fake.commands
            if wrkslots_action(command, "recover")
            and "--ownerless-validate-cargo-home" in command
        ]
        self.assertEqual(1, len(commands))

    def require_read_only_entry(self, *, run: start_unit.Runner | None = None) -> str:
        prospective = "validate-fresh-aaaaaaaaaaaa-read-only"
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            start_unit.require_validate_entry_cleanup(
                self.root,
                prospective_slot=prospective,
                prospective_checkout=self.root / "worktrees/validate" / prospective,
                run=run or self.fake,
                tool_root=self.root,
            )
        return error.getvalue()

    def test_sweep_retains_cargo_home_named_by_multiple_run_records(self) -> None:
        first_checkout, first_cargo, _first_record = self.in_place_cleanup_record(
            "shared-cargo-first", temporary=False, current=True
        )
        _second_checkout, second_cargo, second_record = self.in_place_cleanup_record(
            "shared-cargo-second", temporary=False, current=True
        )
        shutil.rmtree(second_cargo)
        second = start_unit.run_registry.read_record(second_record)
        second["cargo_home"] = str(first_cargo)
        start_unit.run_registry.write_record(second_record, second)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed_cargo_homes"])
        self.assertTrue(first_checkout.exists())
        self.assertTrue(first_cargo.exists())
        self.assertTrue(
            any(
                row.get("cleanup_failed") is True
                and "multiple validation handles" in row["reason"]
                for row in report["retained"]
            ),
            report,
        )

    def test_unsupported_handle_still_vetoes_shared_cargo_home_cleanup(self) -> None:
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "shared-cargo-current", temporary=False, current=True
        )
        unsupported = (
            self.root
            / "ignored/validate/runs/validate-shared-cargo-unsupported.json"
        )
        unsupported.write_text(
            json.dumps(
                {
                    "schema_version": 999,
                    "cargo_home": str(cargo_home),
                    "state": "completed",
                }
            )
            + "\n"
        )

        cleaned, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            start_unit.run_registry.read_record(record),
            record_path=record,
            run=self.fake,
            tool_root=self.root,
        )

        self.assertFalse(cleaned)
        self.assertIn("not owned exclusively", reason or "")
        self.assertTrue(cargo_home.exists())

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed_cargo_homes"])
        self.assertTrue(cargo_home.exists())
        self.assertTrue(
            any(
                row.get("cleanup_failed") is True
                and "multiple validation handles" in row["reason"]
                for row in report["retained"]
            ),
            report,
        )

    def test_sweep_retains_cargo_when_recorded_checkout_is_unclassifiable(self) -> None:
        checkout, cargo_home, _record = self.in_place_cleanup_record(
            "unclassifiable-checkout-cargo",
            temporary=True,
            current=True,
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed_cargo_homes"])
        self.assertTrue(checkout.exists())
        self.assertTrue(cargo_home.exists())
        self.assertTrue(
            any(
                row["checkout"] == str(cargo_home)
                and "associated validation checkout" in row["reason"]
                for row in report["deferred"]
            ),
            report,
        )

    def test_direct_cargo_cleanup_retains_while_checkout_still_exists(self) -> None:
        checkout = (
            self.root
            / "worktrees/validate/validate-fresh-direct-cargo-coupling"
        )
        checkout.mkdir(parents=True)
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes/validate-cargo-direct-coupling"
        )
        cargo_home.mkdir(parents=True)
        record_path = (
            self.root
            / "ignored/validate/runs/validate-direct-cargo-coupling.json"
        )
        record = {
            "schema_version": 1,
            "checkout": str(checkout),
            "temporary_checkout": True,
            "cargo_home": str(cargo_home),
            "state": "completed",
            "exit_code": 0,
            "final_validate_status": "PASSED",
        }
        start_unit.run_registry.write_record(record_path, record)

        removed, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            record,
            record_path=record_path,
            run=self.fake,
            tool_root=self.root,
        )

        self.assertFalse(removed)
        self.assertIn("associated validation checkout still exists", reason or "")
        self.assertTrue(checkout.exists())
        self.assertTrue(cargo_home.exists())
        self.assertFalse(
            any(
                "--ownerless-validate-cargo-home" in command
                for command in self.fake.commands
            )
        )

    def test_entry_authority_ignores_entire_disjoint_cleanup_backlog(self) -> None:
        stale = [
            self.managed_cleanup_record(f"entry-backlog-{index:03d}")[0]
            for index in range(130)
        ]

        started = time.monotonic()
        rc, _output, error = self.invoke()
        elapsed = time.monotonic() - started

        self.assertEqual(0, rc, error)
        self.assertLess(elapsed, 1.0)
        self.assertEqual(0, sum(not path.exists() for path in stale))
        self.assertEqual(130, sum(path.exists() for path in stale))
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=130", error)
        self.assertNotIn("122 deferred", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )

    def test_entry_retains_failed_ledger_records_and_failed_ninth_run(self) -> None:
        self.fake.collected_unit = True
        for index in range(9):
            fresh, record = self.current_unknown_cleanup_record(
                f"entry-ledger-{index:03d}",
                {
                    "state": "refused", "recorded_at": "2026-09-04T12:00:00+00:00",
                    "reason": "stale-base", "exit_code": 3,
                },
            )
            record.update(
                materialized_target=True,
                ledger_mirror={
                    **publication_report(SHA),
                    "recorded_at": "2026-09-04T12:01:00+00:00",
                    "state": "derived-failed",
                    "detail": "no unique canonical terminal",
                    "raw_publication": {"attempts": 0, "published": 0},
                    "scorecard": {"state": "failed", "error": "no unique canonical terminal"},
                },
            )
            if index == 8:
                # The ninth retained checkout ran seven nodes and FAILED.
                # Read-only admission must preserve that negative evidence.
                record.update(
                    state="completed", result="failure", materialized_target=False,
                    admission_result={"state": "admitted", "run_number": 1900,
                                      "recorded_at": "2026-09-04T12:00:00+00:00"},
                    final_validate_status="FAILED", exit_code=1,
                    service_result_schema=start_unit.service_result.SCHEMA_VERSION,
                    selection_mode="only",
                    executed_nodes=7, executed_tests=None, passed_tests=None, detail=None,
                    canonical_verdict="NOT-VALIDATED", scorecard_writeback=None,
                )
            start_unit.run_registry.write_record(
                self.root / "ignored/validate/runs" / f"validate-entry-ledger-{index:03d}.json",
                record,
            )
            (fresh / "retained-receipt.jsonl").write_text("failed execution evidence\n")
        before = self.filesystem_snapshot(self.root)

        def forbidden(*_args: object, **_kwargs: object) -> object:
            raise AssertionError("read-only classification attempted publication or disposal")

        with mock.patch.object(
            start_unit, "publish_recorded_scorecard_handoff", side_effect=forbidden
        ), mock.patch.object(
            start_unit, "archive_orphaned_receipts", side_effect=forbidden
        ), mock.patch.object(
            start_unit, "remove_fresh_checkouts_batch", side_effect=forbidden
        ), mock.patch.object(
            start_unit.run_registry, "update_record", side_effect=forbidden
        ):
            error = self.require_read_only_entry()

        self.assertEqual(before, self.filesystem_snapshot(self.root))
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=9", error)
        self.assertFalse(
            any(
                command[:3] == ["systemctl", "--user", "show"]
                for command in self.fake.commands
            )
        )

    def test_entry_ignores_malformed_disjoint_ninth_managed_group(self) -> None:
        for index in range(8):
            self.managed_cleanup_record(f"entry-ninth-{index:03d}")
        _fresh, ninth = self.managed_cleanup_record("entry-ninth-008")
        original = start_unit.run_registry.read_record(ninth)
        for changes in (
            {"state": "unknown"},
            {"wrkslots_generation": None},
            {"wrkslots_generation": True},
            {"wrkslots_slot": "another-slot"},
            {"schema_version": "1"},
        ):
            with self.subTest(changes=changes):
                ninth.write_text(json.dumps({**original, **changes}) + "\n")
                before = self.filesystem_snapshot(self.root)
                error = self.require_read_only_entry()
                self.assertIn("ENTRY-AUTHORITY state=ready", error)
                self.assertIn("run-handles=9", error)
                self.assertEqual(before, self.filesystem_snapshot(self.root))

        start_unit.run_registry.write_record(ninth, original)
        contradictory = ninth.with_name("validate-entry-ninth-conflicting.json")
        start_unit.run_registry.write_record(
            contradictory,
            {**original, "wrkslots_generation": original["wrkslots_generation"] + 1},
        )
        before = self.filesystem_snapshot(self.root)
        error = self.require_read_only_entry()
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=10", error)
        self.assertEqual(before, self.filesystem_snapshot(self.root))

    def test_entry_budget_expiry_after_first_page_refuses_without_writes(self) -> None:
        for index in range(9):
            self.managed_cleanup_record(f"entry-expiry-{index:03d}")
        before = self.filesystem_snapshot(self.root)
        elapsed = [0.0]
        original_read = start_unit.read_cleanup_record_object

        def read_then_expire(path: Path) -> dict[str, object]:
            result = original_read(path)
            if path.name.endswith("007.json"):
                elapsed[0] = start_unit.VALIDATE_ENTRY_CLASSIFICATION_SECONDS
            return result

        with mock.patch.object(start_unit.time, "monotonic", side_effect=lambda: elapsed[0]), \
             mock.patch.object(
                 start_unit,
                 "read_cleanup_record_object",
                 side_effect=read_then_expire,
             ):
            rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertIn("classification budget expired", error)
        self.assertIn("entry-expiry-008", error)
        self.assertEqual(before, self.filesystem_snapshot(self.root))
        self.assertFalse(any(wrkslots_action(command, "create")
                             for command in self.fake.commands))

    def test_entry_child_timeout_and_late_result_refuse_before_creation(self) -> None:
        for index in range(9):
            self.managed_cleanup_record(f"entry-timeout-{index:03d}")
        before = self.filesystem_snapshot(self.root)
        for late_result in (False, True):
            with self.subTest(late_result=late_result):
                elapsed = [0.0]

                def run(command, **kwargs):
                    if wrkslots_action(command, "classify-create-journals"):
                        self.assertGreater(kwargs["timeout"], 0)
                        self.assertLessEqual(kwargs["timeout"], 30)
                        if late_result:
                            elapsed[0] = 30.0
                        else:
                            raise subprocess.TimeoutExpired(
                                command, kwargs["timeout"]
                            )
                    return self.fake(command, **kwargs)

                with mock.patch.object(start_unit.time, "monotonic", side_effect=lambda: elapsed[0]):
                    rc, _output, error = self.invoke(run=run)
                self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
                self.assertIn("unresolved storage remains retained", error)
                self.assertEqual(before, self.filesystem_snapshot(self.root))
                self.assertFalse(any(wrkslots_action(command, "create")
                                     for command in self.fake.commands))

    def test_entry_budget_preserves_tighter_child_timeout(self) -> None:
        observed = []
        elapsed = [0.0]

        def run(command, **kwargs):
            observed.append(kwargs["timeout"])
            return completed(command)

        with mock.patch.object(start_unit.time, "monotonic", side_effect=lambda: elapsed[0]):
            budget = start_unit.CleanupClassificationBudget(30.0, run)
            budget.run(["probe"], timeout=2.0)
            elapsed[0] = 29.0
            budget.run(["probe"], timeout=2.0)
        self.assertEqual([2.0, 1.0], observed)

    # Event-scaled classification budget,
    # https://github.com/rrnewton/dev-hermit/issues/408. The create-journal
    # classification replays the whole wrkslots event chain, so a fixed 30 s
    # refused ordinary launches once the chain had grown past it.

    def plant_wrkslots_events(self, count: int, machine: str = "devbig014") -> Path:
        shard = self.root / "worktrees" / f"EVENTS.{machine}"
        shard.mkdir(parents=True, exist_ok=True)
        for sequence in range(1, count + 1):
            (shard / f"{sequence:020d}.json").touch()
        return shard

    def test_entry_classification_seconds_scale_with_events_and_stay_finite(
        self,
    ) -> None:
        self.assertEqual(30.0, start_unit.VALIDATE_ENTRY_CLASSIFICATION_SECONDS)
        self.assertEqual(
            0.004, start_unit.VALIDATE_ENTRY_CLASSIFICATION_SECONDS_PER_EVENT
        )
        self.assertEqual(300.0, start_unit.VALIDATE_ENTRY_CLASSIFICATION_MAX_SECONDS)
        cases = (
            (-5, 30.0),
            (0, 30.0),
            (2000, 38.0),
            # The devbig014 chain on 2026-09-29, when a fixed 30 s refused.
            (16273, 95.092),
            (67500, 300.0),
            (67501, 300.0),
            (10**9, 300.0),
        )
        for events, seconds in cases:
            with self.subTest(events=events):
                self.assertAlmostEqual(
                    seconds, start_unit.entry_classification_seconds(events)
                )

    def test_count_wrkslots_events_counts_only_complete_events_in_real_shards(
        self,
    ) -> None:
        self.assertEqual(0, start_unit.count_wrkslots_events(self.root))
        first = self.plant_wrkslots_events(3, "hosta")
        self.plant_wrkslots_events(2, "hostb")
        (first / "00000000000000000004.json.tmp.123").touch()
        (first / "README").touch()
        (first / "1.json").touch()
        (self.root / "worktrees" / "EVENTS.file").touch()
        (self.root / "worktrees" / "EVENTS.link").symlink_to(first)
        (self.root / "worktrees" / "OTHER.hostc").mkdir()
        (self.root / "worktrees" / "OTHER.hostc" / f"{1:020d}.json").touch()
        self.assertEqual(5, start_unit.count_wrkslots_events(self.root))

    def test_entry_admits_a_classification_longer_than_the_fixed_part(self) -> None:
        # 2,000 events: 30 s + 4 ms x 2,000 = 38 s. The fixed 30 s budget
        # refused this 35 s classification as "budget expired".
        self.managed_cleanup_record("event-budget-admit")
        self.plant_wrkslots_events(2000)
        elapsed = [0.0]
        observed: list[float] = []

        def run(command, **kwargs):
            if wrkslots_action(command, "classify-create-journals"):
                observed.append(kwargs["timeout"])
                elapsed[0] += 35.0
            return self.fake(command, **kwargs)

        before = self.filesystem_snapshot(self.root)
        with mock.patch.object(
            start_unit.time, "monotonic", side_effect=lambda: elapsed[0]
        ):
            error = self.require_read_only_entry(run=run)
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("classification=35.000s", error)
        self.assertIn("wrkslots-events=2000 classification-budget=38.0s", error)
        self.assertNotIn("budget expired", error)
        self.assertEqual(1, len(observed), error)
        self.assertAlmostEqual(38.0, observed[0])
        self.assertEqual(before, self.filesystem_snapshot(self.root))

    def test_entry_child_past_the_scaled_budget_still_refuses_naming_it(
        self,
    ) -> None:
        self.managed_cleanup_record("event-budget-refuse")
        self.plant_wrkslots_events(2000)
        before = self.filesystem_snapshot(self.root)
        for late_result in (False, True):
            with self.subTest(late_result=late_result):
                elapsed = [0.0]

                def run(command, **kwargs):
                    if wrkslots_action(command, "classify-create-journals"):
                        self.assertAlmostEqual(38.0, kwargs["timeout"])
                        if late_result:
                            elapsed[0] = 38.0
                        else:
                            raise subprocess.TimeoutExpired(
                                command, kwargs["timeout"]
                            )
                    return self.fake(command, **kwargs)

                with (
                    mock.patch.object(
                        start_unit.time, "monotonic", side_effect=lambda: elapsed[0]
                    ),
                    self.assertRaises(start_unit.ValidateEntryCleanupRefused) as raised,
                ):
                    self.require_read_only_entry(run=run)
                message = str(raised.exception)
                self.assertIn(
                    "timed out after 38s" if not late_result
                    else "classification budget expired while",
                    message,
                )
                self.assertIn(
                    "; unresolved storage remains retained; classification "
                    "budget 38.0s = 30s + 4ms x 2000 wrkslots events, cap 300s",
                    message,
                )
                self.assertNotIn("REACHED", message)
                self.assertEqual(before, self.filesystem_snapshot(self.root))

    def test_entry_budget_cap_bounds_a_hung_child_and_names_the_cap(self) -> None:
        elapsed = [0.0]
        observed: list[float] = []

        def run(command, **kwargs):
            if wrkslots_action(command, "classify-create-journals"):
                observed.append(kwargs["timeout"])
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])
            return self.fake(command, **kwargs)

        with (
            mock.patch.object(
                start_unit, "count_wrkslots_events", return_value=10**9
            ),
            mock.patch.object(
                start_unit.time, "monotonic", side_effect=lambda: elapsed[0]
            ),
            self.assertRaises(start_unit.ValidateEntryCleanupRefused) as raised,
        ):
            self.require_read_only_entry(run=run)
        self.assertEqual([300.0], observed)
        message = str(raised.exception)
        self.assertIn("timed out after 300s", message)
        self.assertIn(
            "classification budget 300.0s = 30s + 4ms x 1000000000 wrkslots "
            "events, cap 300s REACHED: the wrkslots event chain has outgrown "
            "the entry bound",
            message,
        )

    # Entry lock waiting, https://github.com/rrnewton/dev-hermit/issues/389.
    # Waiting for the registry locks is bounded by its own
    # VALIDATE_ENTRY_LOCK_WAIT_SECONDS and is never charged to the read-only
    # classification budget. The bounds are injected so no case sleeps 30 s;
    # each holder is a separate exec'd process holding the real kernel lock.

    ENTRY_LOCK_WAIT_SLOT = "validate-fresh-aaaaaaaaaaaa-lock-wait"

    def entry_lock_identity(self, kind: str) -> tuple[str, Path]:
        canonical = self.root.resolve()
        if kind == "exclusive-generation":
            return (
                "run-writer generation lock",
                canonical / start_unit.run_registry.RUN_WRITER_GENERATION_LOCK,
            )
        self.assertEqual("population", kind)
        return "run-population lock", canonical

    def require_entry_under_bounds(
        self,
        *,
        classification: float,
        lock_wait: float,
        with_authority: bool = False,
        run: start_unit.Runner | None = None,
    ) -> str:
        """Run read-only entry under injected bounds; return its stderr.

        ``with_authority`` is the production shape: the caller already holds
        the shared generation side and entry takes only the population lock.
        A refusal propagates with the captured stderr attached as a note.
        """

        prospective = self.ENTRY_LOCK_WAIT_SLOT
        error = io.StringIO()
        with (
            mock.patch.object(
                start_unit, "VALIDATE_ENTRY_CLASSIFICATION_SECONDS", classification
            ),
            mock.patch.object(
                start_unit, "VALIDATE_ENTRY_LOCK_WAIT_SECONDS", lock_wait
            ),
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_POLL_SECONDS", 0.005),
            contextlib.redirect_stderr(error),
            contextlib.ExitStack() as authority,
        ):
            generation = (
                authority.enter_context(
                    start_unit.run_registry.run_writer_generation_lock(self.root)
                )
                if with_authority
                else None
            )
            try:
                start_unit.require_validate_entry_cleanup(
                    self.root,
                    prospective_slot=prospective,
                    prospective_checkout=self.root / "worktrees/validate" / prospective,
                    run=run or self.fake,
                    tool_root=self.root,
                    generation_authority=generation,
                )
            except BaseException as refusal:
                refusal.add_note(error.getvalue())
                raise
        return error.getvalue()

    def entry_authority_seconds(self, error: str) -> dict[str, float]:
        match = re.search(
            r"ENTRY-AUTHORITY state=ready .* elapsed=([0-9.]+)s "
            r"classification=(-?[0-9.]+)s lock-wait=([0-9.]+)s",
            error,
        )
        self.assertIsNotNone(match, error)
        assert match is not None
        return {
            "elapsed": float(match.group(1)),
            "classification": float(match.group(2)),
            "lock_wait": float(match.group(3)),
        }

    def test_entry_lock_wait_longer_than_budget_is_not_charged_to_it(self) -> None:
        self.managed_cleanup_record("entry-lock-wait-admit")
        cases = (
            ("exclusive-generation", False),
            ("population", False),
            ("population", True),
        )
        for kind, with_authority in cases:
            with self.subTest(kind=kind, with_authority=with_authority):
                lock_name, lock_path = self.entry_lock_identity(kind)
                holder = self.foreign_cleanup_lock_holder(kind, seconds=1.5)
                started = time.monotonic()
                error = self.require_entry_under_bounds(
                    classification=0.5,
                    lock_wait=30.0,
                    with_authority=with_authority,
                )
                wall = time.monotonic() - started
                holder.wait(timeout=5)

                self.assertNotIn("budget expired", error)
                seconds = self.entry_authority_seconds(error)
                # The wait alone is longer than the whole budget, and the
                # classification that was charged stayed inside it.
                self.assertGreaterEqual(seconds["lock_wait"], 1.0, error)
                self.assertGreater(seconds["lock_wait"], 0.5, error)
                self.assertLess(seconds["classification"], 0.5, error)
                self.assertGreaterEqual(seconds["classification"], 0.0, error)
                self.assertGreaterEqual(wall, seconds["lock_wait"])
                self.assertIn("run-handles=1", error)
                self.assertIn("ENTRY-LOCK-WAIT waiting up to ", error)
                waited = re.search(
                    r"ENTRY-LOCK-WAIT waited ([0-9.]+)s for the (.+?) on (.+?), "
                    r"held by (.+)",
                    error,
                )
                self.assertIsNotNone(waited, error)
                assert waited is not None
                self.assertEqual(lock_name, waited.group(2))
                self.assertEqual(str(lock_path), waited.group(3))
                self.assertIn(f"pid {holder.pid} (write: ", waited.group(4))
                self.assertGreaterEqual(float(waited.group(1)), 1.0)

    def test_entry_lock_wait_past_its_bound_refuses_naming_lock_and_wait(self) -> None:
        self.managed_cleanup_record("entry-lock-wait-expired")
        cases = (
            ("exclusive-generation", False),
            ("population", False),
            ("population", True),
        )
        for kind, with_authority in cases:
            with self.subTest(kind=kind, with_authority=with_authority):
                lock_name, lock_path = self.entry_lock_identity(kind)
                holder = self.foreign_cleanup_lock_holder(kind, seconds=60)
                before = self.filesystem_snapshot(self.root)
                commands_before = len(self.fake.commands)
                started = time.monotonic()
                with (
                    mock.patch.object(
                        start_unit,
                        "_prospective_run_handle_conflicts",
                        wraps=start_unit._prospective_run_handle_conflicts,
                    ) as classify,
                    self.assertRaises(start_unit.ValidateEntryLockWaitExpired) as raised,
                ):
                    self.require_entry_under_bounds(
                        classification=30.0,
                        lock_wait=0.4,
                        with_authority=with_authority,
                    )
                wall = time.monotonic() - started
                message = str(raised.exception)

                self.assertIsNone(holder.poll(), "the holder must still hold the lock")
                self.assertTrue(
                    message.startswith("validation entry lock wait bound expired: waited "),
                    message,
                )
                waited = re.match(
                    r"validation entry lock wait bound expired: waited ([0-9.]+)s "
                    r"for the (.+?) on (.+?), held by ",
                    message,
                )
                self.assertIsNotNone(waited, message)
                assert waited is not None
                self.assertGreaterEqual(float(waited.group(1)), 0.4, message)
                self.assertLess(float(waited.group(1)), 5.0, message)
                self.assertLess(wall, 5.0, message)
                self.assertEqual(lock_name, waited.group(2))
                self.assertEqual(str(lock_path), waited.group(3))
                self.assertIn(f"held by pid {holder.pid} (write: ", message)
                self.assertIn(
                    "entry waits at most 0.4s in total for registry locks", message
                )
                self.assertIn(
                    "create-journal and run-handle classification did not start",
                    message,
                )
                self.assertNotIn("classification budget expired", message)
                self.assertNotIn("authority could not be proved", message)
                classify.assert_not_called()
                # Journal classification runs under the locks too, so an
                # expired wait never asked the provider for it.
                self.assertFalse(
                    any(
                        wrkslots_action(command, "classify-create-journals")
                        for command in self.fake.commands[commands_before:]
                    ),
                    self.fake.commands[commands_before:],
                )
                self.assertEqual(before, self.filesystem_snapshot(self.root))
                holder.terminate()
                holder.wait(timeout=5)

    def test_entry_slow_classification_after_lock_wait_is_still_refused(self) -> None:
        self.managed_cleanup_record("entry-lock-wait-slow")
        original_read = start_unit.read_cleanup_record_object

        def slow_read(path: Path) -> dict[str, object]:
            time.sleep(0.7)
            return original_read(path)

        cases = (
            ("exclusive-generation", False),
            ("population", False),
            ("population", True),
        )
        for kind, with_authority in cases:
            with self.subTest(kind=kind, with_authority=with_authority):
                holder = self.foreign_cleanup_lock_holder(kind, seconds=1.0)
                with (
                    mock.patch.object(
                        start_unit, "read_cleanup_record_object", side_effect=slow_read
                    ),
                    self.assertRaises(start_unit.ValidateEntryCleanupRefused) as raised,
                ):
                    self.require_entry_under_bounds(
                        classification=0.5,
                        lock_wait=30.0,
                        with_authority=with_authority,
                    )
                holder.wait(timeout=5)
                message = str(raised.exception)
                captured = "\n".join(getattr(raised.exception, "__notes__", []))

                self.assertNotIsInstance(
                    raised.exception, start_unit.ValidateEntryLockWaitExpired
                )
                self.assertIn(
                    "read-only entry classification budget expired while "
                    "finishing run-handle classification; unresolved storage "
                    "remains retained",
                    message,
                )
                # The lock really was contended first, and the refusal reports
                # the wait separately from the classification it charged.
                self.assertIn("ENTRY-LOCK-WAIT waited ", captured)
                split = re.search(
                    r"s \((-?[0-9.]+)s classifying, ([0-9.]+)s waiting for "
                    r"registry locks\)",
                    message,
                )
                self.assertIsNotNone(split, message)
                assert split is not None
                self.assertGreaterEqual(float(split.group(1)), 0.5, message)
                self.assertGreater(float(split.group(2)), 0.1, message)

    def test_entry_budget_charges_journal_and_handle_classification_after_the_wait(
        self,
    ) -> None:
        # Both steps now run after the locks are granted: 0.3 s of create-journal
        # classification plus 0.3 s of run-handle classification exceeds a 0.5 s
        # budget. A budget that left journal classification uncharged (or ran it
        # before the budget started) would charge 0.3 s and admit this.
        self.managed_cleanup_record("entry-lock-wait-paused")
        original_read = start_unit.read_cleanup_record_object

        def slow_journals(command, **kwargs):
            if wrkslots_action(command, "classify-create-journals"):
                time.sleep(0.3)
            return self.fake(command, **kwargs)

        def slow_read(path: Path) -> dict[str, object]:
            time.sleep(0.3)
            return original_read(path)

        for kind in ("exclusive-generation", "population"):
            with self.subTest(kind=kind):
                holder = self.foreign_cleanup_lock_holder(kind, seconds=1.0)
                with (
                    mock.patch.object(
                        start_unit, "read_cleanup_record_object", side_effect=slow_read
                    ),
                    self.assertRaises(start_unit.ValidateEntryCleanupRefused) as raised,
                ):
                    self.require_entry_under_bounds(
                        classification=0.5, lock_wait=30.0, run=slow_journals
                    )
                holder.wait(timeout=5)
                captured = "\n".join(getattr(raised.exception, "__notes__", []))
                self.assertNotIsInstance(
                    raised.exception, start_unit.ValidateEntryLockWaitExpired
                )
                self.assertIn(
                    "read-only entry classification budget expired while "
                    "finishing run-handle classification",
                    str(raised.exception),
                )
                self.assertIn("ENTRY-LOCK-WAIT waited ", captured)

    def test_validate_run_refuses_lock_wait_past_bound_with_its_own_remedy(self) -> None:
        holder = self.foreign_cleanup_lock_holder("population", seconds=60)
        started = time.monotonic()
        with (
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_WAIT_SECONDS", 0.3),
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_POLL_SECONDS", 0.005),
        ):
            rc, _output, error = self.invoke()
        wall = time.monotonic() - started

        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertLess(wall, 10.0, error)
        self.assertIsNone(holder.poll(), "the holder must still hold the lock")
        self.assertIn(
            "validation entry lock wait bound expired: waited ", error
        )
        self.assertIn(
            f"for the run-population lock on {self.root.resolve()}, held by "
            f"pid {holder.pid} (write: ",
            error,
        )
        self.assertIn("entry waits at most 0.3s in total for registry locks", error)
        self.assertIn(
            "another process held a validation registry lock past the entry "
            "lock-wait bound and no checkout was created",
            error,
        )
        self.assertNotIn("classification budget expired", error)
        self.assertNotIn("inspect the named overlapping slot", error)
        self.assertFalse(any(wrkslots_action(command, "create")
                             for command in self.fake.commands))

    def test_validate_run_admits_after_a_lock_wait_longer_than_budget(self) -> None:
        self.foreign_cleanup_lock_holder("population", seconds=1.5)
        with (
            mock.patch.object(start_unit, "VALIDATE_ENTRY_CLASSIFICATION_SECONDS", 0.5),
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_POLL_SECONDS", 0.005),
        ):
            rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertNotIn("budget expired", error)
        seconds = self.entry_authority_seconds(error)
        self.assertGreaterEqual(seconds["lock_wait"], 1.0, error)
        self.assertLess(seconds["classification"], 0.5, error)
        self.assertIn("ENTRY-LOCK-WAIT waited ", error)
        self.assertTrue(any(wrkslots_action(command, "create")
                            for command in self.fake.commands))

    def test_describe_flock_holders_names_holders_not_blocked_waiters(self) -> None:
        holder = self.foreign_cleanup_lock_holder("population", seconds=60)
        waiter = subprocess.Popen(
            [
                sys.executable,
                "-c",
                "import fcntl, os, sys\n"
                "fd = os.open(sys.argv[1], os.O_RDONLY | os.O_DIRECTORY)\n"
                "fcntl.flock(fd, fcntl.LOCK_EX)\n",
                str(self.root),
            ],
        )

        def stop_waiter() -> None:
            if waiter.poll() is None:
                waiter.kill()
            waiter.wait(timeout=5)

        self.addCleanup(stop_waiter)
        deadline = time.monotonic() + 10
        while not any(
            "->" in line and f" {waiter.pid} " in line
            for line in Path("/proc/locks").read_text().splitlines()
        ):
            self.assertLess(time.monotonic(), deadline, "waiter never blocked")
            self.assertIsNone(waiter.poll())
            time.sleep(0.01)

        described = start_unit.describe_flock_holders(self.root)
        self.assertIn(f"pid {holder.pid} (write: ", described)
        self.assertNotIn(f"pid {waiter.pid} ", described)
        self.assertNotIn("unidentified", described)

        unlocked = self.root / "unlocked-file"
        unlocked.write_text("")
        self.assertIn(
            "an unidentified holder (/proc/locks lists no flock on",
            start_unit.describe_flock_holders(unlocked),
        )

    # In-process contention, https://github.com/rrnewton/dev-hermit/issues/389.
    # Entry polls nonblocking acquisitions, so it must keep waiting where the
    # blocking form waited (another thread that will release) and keep raising
    # where the blocking form raised (conflicts no wait can end).

    def hold_population_on_thread(
        self, *, seconds: float, reject_same_process: bool = False
    ) -> tuple[threading.Thread, Callable[[], None]]:
        """Hold real run-population authority on another thread of this process."""

        held = threading.Event()
        release = threading.Event()
        failures: list[BaseException] = []

        def hold() -> None:
            try:
                with start_unit.run_registry.run_population_lock(
                    self.root, reject_same_process=reject_same_process
                ):
                    held.set()
                    release.wait(seconds)
            except BaseException as error:  # surfaced by the cleanup below
                failures.append(error)
                held.set()

        holder = threading.Thread(target=hold, name="population-holder")
        holder.start()

        def stop() -> None:
            release.set()
            holder.join(timeout=10)
            self.assertFalse(holder.is_alive(), "population holder thread hung")
            self.assertEqual([], failures)

        # tearDown deletes the state root and cleanups run after it, so each
        # caller stops the holder itself before returning.
        self.assertTrue(held.wait(timeout=10), "holder thread never took the lock")
        self.assertEqual([], failures)
        return holder, stop

    def test_entry_waits_for_another_thread_and_counts_it_as_lock_wait(self) -> None:
        self.managed_cleanup_record("entry-thread-wait")
        for with_authority in (False, True):
            with self.subTest(with_authority=with_authority):
                _holder, stop = self.hold_population_on_thread(seconds=0.8)
                try:
                    error = self.require_entry_under_bounds(
                        classification=0.5,
                        lock_wait=30.0,
                        with_authority=with_authority,
                    )
                finally:
                    stop()

                self.assertNotIn("budget expired", error)
                seconds = self.entry_authority_seconds(error)
                self.assertGreaterEqual(seconds["lock_wait"], 0.5, error)
                self.assertLess(seconds["classification"], 0.5, error)
                self.assertIn(
                    "ENTRY-LOCK-WAIT waited ", error
                )
                self.assertIn(
                    f"for the run-population lock on {self.root.resolve()}, held "
                    f"by another thread of this process (pid {os.getpid()})",
                    error,
                )

    def assert_entry_refused_at_once(self, *, expected: str) -> None:
        # A 5 s bound: retrying a conflict that cannot end would spend all of
        # it and then report a lock-wait expiry instead of this conflict.
        started = time.monotonic()
        with self.assertRaises(start_unit.ValidateEntryCleanupRefused) as raised:
            self.require_entry_under_bounds(classification=30.0, lock_wait=5.0)
        wall = time.monotonic() - started
        message = str(raised.exception)
        captured = "\n".join(getattr(raised.exception, "__notes__", []))
        self.assertNotIsInstance(
            raised.exception, start_unit.ValidateEntryLockWaitExpired, message
        )
        self.assertIn("authority could not be proved after ", message)
        self.assertIn(expected, message)
        self.assertNotIn("ENTRY-LOCK-WAIT", captured)
        self.assertLess(wall, 2.0, message)

    def test_entry_raises_at_once_for_same_thread_reentry(self) -> None:
        self.managed_cleanup_record("entry-same-thread")
        with start_unit.run_registry.run_population_lock(self.root):
            self.assert_entry_refused_at_once(
                expected="this process already holds run-population authority for "
                f"{self.root.resolve()}"
            )
        with start_unit.run_registry.run_writer_generation_lock(
            self.root, exclusive=True
        ):
            self.assert_entry_refused_at_once(
                expected="this process already holds exclusive run-writer "
                "generation authority"
            )

    def test_entry_raises_at_once_for_a_thread_that_rejects_waiters(self) -> None:
        self.managed_cleanup_record("entry-rejecting-thread")
        holder, stop = self.hold_population_on_thread(
            seconds=60, reject_same_process=True
        )
        try:
            self.assert_entry_refused_at_once(
                expected="this process already holds run-population authority for "
                f"{self.root.resolve()}"
            )
            self.assertTrue(holder.is_alive(), "the rejecting holder must still hold")
        finally:
            stop()

    @unittest.skipUnless(hasattr(os, "fork"), "requires POSIX fork")
    def test_entry_raises_at_once_in_a_fork_that_inherited_the_locks(self) -> None:
        import select

        self.managed_cleanup_record("entry-fork")
        read_fd, write_fd = os.pipe()
        child: int | None = None
        try:
            with start_unit.run_registry.run_population_lock(self.root):
                child = os.fork()
                if child == 0:
                    os.close(read_fd)
                    report: dict[str, object] = {}
                    try:
                        started = time.monotonic()
                        try:
                            self.require_entry_under_bounds(
                                classification=30.0, lock_wait=5.0
                            )
                        except BaseException as error:
                            report = {
                                "type": type(error).__name__,
                                "message": str(error),
                                "notes": "\n".join(getattr(error, "__notes__", [])),
                            }
                        else:
                            report = {"type": "admitted"}
                        report["wall"] = time.monotonic() - started
                        os.write(write_fd, json.dumps(report).encode())
                    finally:
                        os._exit(0)
                os.close(write_fd)
                ready, _writable, _exceptional = select.select([read_fd], [], [], 4.0)
                self.assertEqual([read_fd], ready, "forked entry kept waiting")
                chunks = []
                while chunk := os.read(read_fd, 65536):
                    chunks.append(chunk)
                waited, status = os.waitpid(child, 0)
                child = None
                self.assertGreater(waited, 0)
                self.assertEqual(0, os.waitstatus_to_exitcode(status))
        finally:
            if child is not None:
                os.kill(child, 9)
                os.waitpid(child, 0)
            with contextlib.suppress(OSError):
                os.close(read_fd)
            with contextlib.suppress(OSError):
                os.close(write_fd)
        report = json.loads(b"".join(chunks))
        self.assertEqual("ValidateEntryCleanupRefused", report["type"], report)
        self.assertIn("forked process inherited run-writer generation", report["message"])
        self.assertNotIn("ENTRY-LOCK-WAIT", report["notes"])
        self.assertLess(report["wall"], 2.0, report)

    def test_entry_lock_wait_bound_is_one_deadline_across_both_locks(self) -> None:
        # 0.3 s on the generation lock, then 0.3 s on population, against a
        # 0.4 s total bound: each wait alone fits, together they do not.
        self.managed_cleanup_record("entry-lock-wait-total")
        holder = self.foreign_cleanup_lock_holder(
            "exclusive-generation-then-population", seconds=0.3
        )
        started = time.monotonic()
        with self.assertRaises(start_unit.ValidateEntryLockWaitExpired) as raised:
            self.require_entry_under_bounds(classification=30.0, lock_wait=0.4)
        wall = time.monotonic() - started
        message = str(raised.exception)
        captured = "\n".join(getattr(raised.exception, "__notes__", []))
        holder.wait(timeout=5)

        self.assertIn(
            f"for the run-population lock on {self.root.resolve()}, held by ", message
        )
        self.assertIn("ENTRY-LOCK-WAIT waited ", captured)
        self.assertIn("for the run-writer generation lock on ", captured)
        total = re.search(r"and waited ([0-9.]+)s in total", message)
        self.assertIsNotNone(total, message)
        assert total is not None
        self.assertGreaterEqual(float(total.group(1)), 0.4, message)
        self.assertLess(float(total.group(1)), 3.0, message)
        self.assertLess(wall, 3.0, message)

    def test_validate_run_lock_wait_bound_spans_the_pre_entry_generation_lock(
        self,
    ) -> None:
        holder = self.foreign_cleanup_lock_holder(
            "exclusive-generation-then-population", seconds=0.3
        )
        with (
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_WAIT_SECONDS", 0.4),
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_POLL_SECONDS", 0.005),
        ):
            rc, _output, error = self.invoke()
        holder.wait(timeout=5)

        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertIn(
            "ENTRY-LOCK-WAIT waited ", error
        )
        self.assertIn("for the run-writer generation lock on ", error)
        self.assertIn(
            f"for the run-population lock on {self.root.resolve()}, held by ", error
        )
        self.assertIn("entry waits at most 0.4s in total for registry locks", error)
        self.assertIn(
            "another process held a validation registry lock past the entry "
            "lock-wait bound and no checkout was created",
            error,
        )
        self.assertFalse(any(wrkslots_action(command, "create")
                             for command in self.fake.commands))

    def test_validate_run_bounds_the_pre_entry_generation_lock_wait(self) -> None:
        holder = self.foreign_cleanup_lock_holder("exclusive-generation", seconds=60)
        with (
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_WAIT_SECONDS", 0.3),
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_POLL_SECONDS", 0.005),
            # The refusal's own no-op same-launch cleanup also wants the
            # exclusive side this holder keeps; do not wait 30 s for it here.
            mock.patch.object(start_unit, "SAME_LAUNCH_CLEANUP_LOCK_WAIT_SECONDS", 0.2),
        ):
            rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertIsNone(holder.poll(), "the holder must still hold the lock")
        expired = re.search(
            r"validation entry lock wait bound expired: waited ([0-9.]+)s for the "
            r"run-writer generation lock on (.+?), held by pid (\d+) \(write: ",
            error,
        )
        self.assertIsNotNone(expired, error)
        assert expired is not None
        self.assertLess(float(expired.group(1)), 5.0, error)
        self.assertEqual(
            str(
                self.root.resolve()
                / start_unit.run_registry.RUN_WRITER_GENERATION_LOCK
            ),
            expired.group(2),
        )
        self.assertEqual(str(holder.pid), expired.group(3))
        self.assertIn(
            "another process held a validation registry lock past the entry "
            "lock-wait bound and no checkout was created",
            error,
        )
        self.assertFalse(any(
            wrkslots_action(command, "classify-create-journals")
            or wrkslots_action(command, "create")
            for command in self.fake.commands
        ))

    def test_entry_classifies_create_journals_under_the_population_lock(self) -> None:
        self.managed_cleanup_record("entry-journals-locked")
        probes: list[str] = []

        def probing(command, **kwargs):
            if wrkslots_action(command, "classify-create-journals"):
                # A second open file description in this process conflicts with
                # the population flock exactly as another process would.
                descriptor = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
                try:
                    fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    probes.append("held")
                else:
                    probes.append("free")
                finally:
                    os.close(descriptor)
            return self.fake(command, **kwargs)

        for with_authority in (False, True):
            with self.subTest(with_authority=with_authority):
                probes.clear()
                error = self.require_entry_under_bounds(
                    classification=30.0,
                    lock_wait=30.0,
                    with_authority=with_authority,
                    run=probing,
                )
                self.assertIn("ENTRY-AUTHORITY state=ready", error)
                self.assertEqual(["held"], probes)

    def test_entry_waits_out_a_live_removal_before_reading_its_journal(self) -> None:
        # Row 1 of the issue: a removal in progress holds the directory lock and
        # its FINISH journal, and clears the journal before it releases.
        self.managed_cleanup_record("entry-live-removal")
        marker = self.root / "live-removal-finish-journal"
        marker.write_text("")
        seen: list[bool] = []

        def removal_aware(command, **kwargs):
            if wrkslots_action(command, "classify-create-journals"):
                seen.append(marker.exists())
                if marker.exists():
                    return completed(
                        command,
                        rc=1,
                        stderr="finish journal of a removal still in progress",
                    )
            return self.fake(command, **kwargs)

        holder = self.foreign_cleanup_lock_holder(
            "population-then-unlink", seconds=0.5, marker=marker
        )
        error = self.require_entry_under_bounds(
            classification=30.0, lock_wait=30.0, run=removal_aware
        )
        holder.wait(timeout=5)

        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("ENTRY-LOCK-WAIT waited ", error)
        self.assertEqual([False], seen)
        self.assertFalse(marker.exists())

    def test_entry_lock_wait_redescribes_a_changed_holder(self) -> None:
        self.managed_cleanup_record("entry-holder-changes")
        cases = (
            # Looked up again while waiting: the change is reported as it happens.
            (0.05, True),
            # Never looked up again while waiting: the expiry still re-describes.
            (100.0, False),
        )
        for interval, reported_while_waiting in cases:
            with self.subTest(interval=interval):
                holder = self.foreign_cleanup_lock_holder("population", seconds=60)
                names = iter(["holder-A"])
                with (
                    mock.patch.object(
                        start_unit,
                        "describe_flock_holders",
                        side_effect=lambda _path: next(names, "holder-B"),
                    ),
                    mock.patch.object(
                        start_unit, "VALIDATE_ENTRY_LOCK_REDESCRIBE_SECONDS", interval
                    ),
                    self.assertRaises(start_unit.ValidateEntryLockWaitExpired) as raised,
                ):
                    self.require_entry_under_bounds(classification=30.0, lock_wait=0.4)
                message = str(raised.exception)
                captured = "\n".join(getattr(raised.exception, "__notes__", []))
                holder.terminate()
                holder.wait(timeout=5)

                self.assertIn(", held by holder-B (earlier holders: holder-A); ", message)
                self.assertIn("ENTRY-LOCK-WAIT waiting up to ", captured)
                self.assertIn("held by holder-A", captured)
                self.assertEqual(
                    reported_while_waiting,
                    "now held by holder-B" in captured,
                    captured,
                )

    def test_entry_lock_grant_names_every_holder_it_waited_for(self) -> None:
        self.managed_cleanup_record("entry-holder-grant")
        self.foreign_cleanup_lock_holder("population", seconds=0.6)
        names = iter(["holder-A"])
        with (
            mock.patch.object(
                start_unit,
                "describe_flock_holders",
                side_effect=lambda _path: next(names, "holder-B"),
            ),
            mock.patch.object(start_unit, "VALIDATE_ENTRY_LOCK_REDESCRIBE_SECONDS", 0.05),
        ):
            error = self.require_entry_under_bounds(classification=30.0, lock_wait=30.0)
        self.assertRegex(
            error,
            r"ENTRY-LOCK-WAIT waited [0-9.]+s for the run-population lock on .+, "
            r"held by holder-A then holder-B\n",
        )

    def test_holder_command_line_read_gives_up_on_a_stuck_read(self) -> None:
        fifo = self.root / "stuck-cmdline"
        os.mkfifo(fifo)

        def release_reader() -> None:
            # The abandoned reader is blocked opening the FIFO; a writer lets
            # it finish so no thread outlives the test.
            with contextlib.suppress(OSError):
                os.close(os.open(fifo, os.O_WRONLY | os.O_NONBLOCK))

        self.addCleanup(release_reader)
        started = time.monotonic()
        with self.assertRaisesRegex(TimeoutError, "did not finish within 0.2s"):
            start_unit._read_proc_bounded(str(fifo), 0.2)
        self.assertLess(time.monotonic() - started, 2.0)

        real_read = start_unit._read_proc_bounded
        with (
            mock.patch.object(start_unit, "VALIDATE_ENTRY_HOLDER_READ_SECONDS", 0.2),
            mock.patch.object(
                start_unit,
                "_read_proc_bounded",
                side_effect=lambda path, timeout: real_read(str(fifo), timeout),
            ) as read,
        ):
            started = time.monotonic()
            summary = start_unit._process_command_summary(os.getpid())
        self.assertLess(time.monotonic() - started, 2.0)
        read.assert_called_once_with(f"/proc/{os.getpid()}/cmdline", 0.2)
        self.assertEqual(
            f"command line unreadable: reading {fifo} did not finish within 0.2s",
            summary,
        )

    def test_cleanup_handle_fifo_is_refused_without_waiting_for_a_writer(self) -> None:
        fifo = self.root / "blocked-handle.json"
        os.mkfifo(fifo)
        probe = subprocess.run(
            [sys.executable, "-c",
             "import sys; from pathlib import Path; sys.path.insert(0, sys.argv[1]); "
             "import start_unit\n"
             "try: start_unit.read_cleanup_record_object(Path(sys.argv[2]))\n"
             "except RuntimeError as error: print(error)\n"
             "else: raise SystemExit('nonregular handle was accepted')\n",
             str(Path(start_unit.__file__).parent), str(fifo)],
            capture_output=True, text=True, timeout=5,
        )
        self.assertEqual(0, probe.returncode, probe.stderr)
        self.assertIn("is not a regular single-link file", probe.stdout)
        self.assertTrue(stat.S_ISFIFO(fifo.stat().st_mode))

    def test_entry_refuses_symlink_run_handle_without_following_it(self) -> None:
        fresh, record = self.managed_cleanup_record("entry-symlink")
        original = record.read_bytes()
        target = self.root / "preserved-handle.json"
        record.rename(target)
        record.symlink_to(target)
        rc, _output, error = self.invoke()
        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertIn(str(record), error)
        self.assertEqual(original, target.read_bytes())
        self.assertTrue(record.is_symlink())
        self.assertTrue(fresh.is_dir())
        self.assertFalse(any(wrkslots_action(command, "create")
                             for command in self.fake.commands))
    def test_entry_refuses_run_handle_claim_overlapping_prospective_slot(self) -> None:
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        prospective = self.root / "worktrees/validate" / prospective_slot
        record = self.root / "ignored/validate/runs/validate-overlap.json"
        record.parent.mkdir(parents=True, exist_ok=True)
        record.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "state": "completed",
                    "checkout": str(prospective),
                    "temporary_checkout": True,
                }
            )
            + "\n"
        )

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("overlaps checkout", error)
        self.assertIn(str(record), error)
        self.assertFalse(prospective.exists() or prospective.is_symlink())
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_refuses_indeterminate_prospective_run_handle_authority(self) -> None:
        broken = self.root / "ignored/validate/runs/validate-unreadable.json"
        broken.parent.mkdir(parents=True, exist_ok=True)
        broken.write_text("{not-json\n")

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("prospective path authority is indeterminate", error)
        self.assertIn(str(broken), error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_refuses_orphan_sidecar_lookalike_as_indeterminate(self) -> None:
        orphan = (
            self.root
            / "ignored/validate/runs/validate-orphan.service-result.json"
        )
        orphan.parent.mkdir(parents=True, exist_ok=True)
        orphan.write_text("{not-json\n")

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("prospective path authority is indeterminate", error)
        self.assertIn(str(orphan), error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_does_not_hide_storage_handle_with_sidecar_suffix(self) -> None:
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        prospective = self.root / "worktrees/validate" / prospective_slot
        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        (runs / "validate-sidecar-owner.json").write_text(
            json.dumps({"checkout": str(self.checkout)}) + "\n"
        )
        collision = runs / "validate-sidecar-owner.service-result.json"
        collision.write_text(json.dumps({"checkout": str(prospective)}) + "\n")

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("overlaps checkout", error)
        self.assertIn(str(collision), error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_refuses_partial_handle_with_sidecar_suffix(self) -> None:
        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        owner = runs / "validate-sidecar-owner.json"
        owner.write_text(
            json.dumps(
                {
                    "checkout": str(self.checkout),
                    "service_result_schema": start_unit.service_result.SCHEMA_VERSION,
                    "target": SHA,
                }
            )
            + "\n"
        )
        partial = start_unit.service_result.result_path(owner)
        partial.write_text(json.dumps({"schema_version": 1, "state": "preparing"}))

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("names no storage path", error)
        self.assertIn(str(partial), error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_ignores_exact_owned_framework_result_sidecar(self) -> None:
        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        owner = runs / "validate-sidecar-owner.json"
        owner.write_text(
            json.dumps(
                {
                    "checkout": str(self.checkout),
                    "service_result_schema": 1,
                    "target": SHA,
                }
            )
            + "\n"
        )
        start_unit.service_result.result_path(owner).write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "commit": SHA,
                    "profile": "full",
                    "final_validate_status": "PASSED",
                    "exit_code": 0,
                    "executed_nodes": 1,
                    "executed_tests": 1,
                }
            )
            + "\n"
        )

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("run-handles=1", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_ignores_owned_framework_result_from_a_prior_target(self) -> None:
        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        owner = runs / "validate-sidecar-prior-target.json"
        owner.write_text(
            json.dumps(
                {
                    "checkout": str(self.checkout),
                    "service_result_schema": 1,
                    "target": SHA,
                }
            )
            + "\n"
        )
        prior_target = "b" * 40
        start_unit.service_result.result_path(owner).write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "commit": prior_target,
                    "profile": "full",
                    "final_validate_status": "PASSED",
                    "exit_code": 0,
                    "executed_nodes": 1,
                    "executed_tests": 1,
                }
            )
            + "\n"
        )

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("run-handles=1", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_refuses_readable_handle_without_storage_authority(self) -> None:
        partial = self.root / "ignored/validate/runs/validate-partial.json"
        partial.parent.mkdir(parents=True, exist_ok=True)
        partial.write_text(json.dumps({"schema_version": 1, "state": "preparing"}))

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("names no storage path", error)
        self.assertIn(str(partial), error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_does_not_hide_storage_handle_with_count_suffix(self) -> None:
        from test_reverie_validate import count_transport_fixture

        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        prospective = self.root / "worktrees/validate" / prospective_slot
        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        owner, owner_record, _binding = count_transport_fixture(runs)
        start_unit.run_registry.write_record(owner, owner_record)
        collision = start_unit.reverie_node.count_sidecar_path(owner)
        collision.write_text(json.dumps({"checkout": str(prospective)}) + "\n")

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("overlaps checkout", error)
        self.assertIn(str(collision), error)

    def test_entry_refuses_partial_handle_with_count_suffix(self) -> None:
        from test_reverie_validate import count_transport_fixture

        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        owner, owner_record, _binding = count_transport_fixture(runs)
        start_unit.run_registry.write_record(owner, owner_record)
        partial = start_unit.reverie_node.count_sidecar_path(owner)
        partial.write_text(json.dumps({"schema_version": 1, "state": "preparing"}))

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("names no storage path", error)
        self.assertIn(str(partial), error)

    def test_entry_ignores_exact_owned_count_sidecar(self) -> None:
        from test_reverie_validate import count_transport_fixture

        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        owner, owner_record, binding = count_transport_fixture(runs)
        start_unit.run_registry.write_record(owner, owner_record)
        sidecar = start_unit.reverie_node.count_sidecar_path(owner)
        start_unit.reverie_node.publish_count_sidecar(sidecar, binding)

        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertIn("run-handles=1", error)

    def test_entry_refuses_create_journal_overlapping_prospective_slot(self) -> None:
        prospective_slot = f"validate-fresh-{SHA[:12]}-{os.getpid()}-12345678"
        prospective = self.root / "worktrees/validate" / prospective_slot
        report = json.loads(
            self.create_journal_report(prospective_slot=prospective_slot)
        )
        journal = report["journals"][0]
        journal["slot"] = prospective_slot
        journal["slot_type"] = "validate"
        journal["slot_path"] = str(prospective)
        journal["checkouts"][0]["path"] = prospective.relative_to(self.root).as_posix()
        self.fake.create_journal_classification_output = json.dumps(report)

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ), mock.patch.object(start_unit.secrets, "token_hex", return_value="12345678"):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("overlaps a journal slot", error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_two_real_main_calls_reach_admitted_payload_concurrently(self) -> None:
        admitted_barrier = threading.Barrier(2)
        release_barrier = threading.Barrier(2)
        entered: list[str] = []
        active_payloads: set[str] = set()
        active_payloads_lock = threading.Lock()
        results: dict[str, int] = {}
        failures: list[BaseException] = []
        cleanup_authority_seen = False
        real_update = start_unit.run_registry.update_record

        def update_after_acceptance(
            path: Path, *, blocking: bool = True, **fields: object
        ) -> dict[str, object]:
            nonlocal cleanup_authority_seen
            updated = real_update(path, blocking=blocking, **fields)
            if fields.get("state") != "running":
                return updated
            name = path.stem.removeprefix("validate-concurrent-")
            with active_payloads_lock:
                self.assertIn(name, active_payloads)
                entered.append(name)
            leader = admitted_barrier.wait(timeout=5)
            if leader == 0:
                with start_unit.destructive_cleanup_authority(self.root):
                    cleanup_authority_seen = True
            release_barrier.wait(timeout=5)
            with active_payloads_lock:
                active_payloads.remove(name)
            return updated

        def launch(name: str) -> None:
            fake = FakeRun(self.checkout)

            def run(command: list[str], **kwargs: object):
                if command[:2] == ["mktemp", "-d"] and "validate-cargo-" in command[2]:
                    private = Path(command[2].replace("XXXXXXXX", name))
                    private.mkdir(parents=True, exist_ok=True)
                    return completed(command, stdout=f"{private}\n")
                if command[0] == "systemd-run" and "validate-lock" in command:
                    with active_payloads_lock:
                        active_payloads.add(name)
                return fake(command, **kwargs)

            try:
                results[name] = start_unit.main(
                    [
                        "--checkout",
                        str(self.checkout),
                        "--agent",
                        f"hermit-{name}",
                        "--target",
                        SHA,
                        "--unit",
                        f"validate-concurrent-{name}",
                        "--log",
                        str(self.root / f"{name}.log"),
                        "--",
                        "full",
                    ],
                    run=run,
                    environment=self.environment,
                    root=self.root,
                    sleep=lambda _seconds: None,
                )
            except BaseException as error:
                failures.append(error)

        with mock.patch.object(
            start_unit.pane_owner, "create_pane", return_value=None
        ), mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=update_after_acceptance,
        ):
            threads = [
                threading.Thread(target=launch, args=(name,))
                for name in ("first", "second")
            ]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(timeout=10)

        self.assertTrue(all(not thread.is_alive() for thread in threads))
        self.assertEqual([], failures)
        self.assertCountEqual(["first", "second"], entered)
        self.assertTrue(cleanup_authority_seen)
        self.assertEqual(set(), active_payloads)
        self.assertEqual({"first": 0, "second": 0}, results)
    def test_entry_cleanup_does_not_probe_or_remove_completed_managed_slot(self) -> None:
        stale, _record = self.managed_cleanup_record("entry-live")
        holder = subprocess.Popen(["sleep", "60"], cwd=stale, text=True)
        self.addCleanup(holder.kill)

        def run(command: list[str], **kwargs: object):
            if wrkslots_action(command, "remove-validate-batch"):
                observed = Path(os.readlink(f"/proc/{holder.pid}/cwd"))
                self.assertEqual(stale, observed)
                self.fake.validate_batch_retained[stale.name] = (
                    f"live process {holder.pid} cwd={observed} uses slot"
                )
            return self.fake(command, **kwargs)

        rc, _output, error = self.invoke(run=run)

        self.assertEqual(0, rc, error)
        self.assertTrue(stale.exists())
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )

    def test_entry_cleanup_preserves_running_checkout_and_allows_concurrency(self) -> None:
        running, _record = self.managed_cleanup_record(
            "entry-running", state="running"
        )

        def run(command: list[str], **kwargs: object):
            if (
                command[:3] == ["systemctl", "--user", "show"]
                and "validate-entry-running.service" in command
            ):
                return completed(
                    command,
                    stdout=(
                        "LoadState=loaded\nActiveState=active\nSubState=running\n"
                        "InvocationID=entry-running-fixture\n"
                    ),
                )
            return self.fake(command, **kwargs)

        rc, _output, error = self.invoke(run=run)

        self.assertEqual(0, rc, error)
        self.assertTrue(running.exists())
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=1", error)
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_ignores_disjoint_running_frozen_singleton_after_admission_tamper(
        self,
    ) -> None:
        frozen = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-entry-running-tampered-admission"
        )
        (frozen / ".git").mkdir(parents=True)
        record_path = (
            self.root
            / "ignored/validate/runs/validate-entry-running-tampered-admission.json"
        )
        record = {
            "schema_version": 1,
            "kind": "frozen-validate",
            "validation_kind": "frozen-validate",
            "unit": "validate-entry-running-tampered-admission.service",
            "checkout": str(frozen),
            "source_checkout": str(self.checkout),
            "temporary_checkout": True,
            "admission": start_unit.FROZEN_RESULT_ADMISSION,
            "repo": "rrnewton/hermit",
            "state": "unknown",
            "result": "unknown",
            "result_source": "historical-validation-service-result",
            "service_result_schema": 3,
            "detail": "historical schema 3 has no current authority",
        }
        start_unit.run_registry.write_record(record_path, record)

        groups, singles, _rows, retained, _discovery = (
            start_unit.discover_cleanup_groups(
                self.root, self.root / "ignored/validate/runs"
            )
        )
        self.assertEqual([], singles)
        self.assertEqual([], retained)
        self.assertIn(str(frozen), groups)
        self.assertFalse(groups[str(frozen)].managed)

        record["admission"] = "ci-hub validate-lock"
        start_unit.run_registry.write_record(record_path, record)
        groups, singles, _rows, retained, _discovery = (
            start_unit.discover_cleanup_groups(
                self.root, self.root / "ignored/validate/runs"
            )
        )
        self.assertEqual({}, groups)
        self.assertEqual([record_path], [row.path for row in singles])
        self.assertEqual([], retained)

        def run(command: list[str], **kwargs: object):
            if (
                command[:3] == ["systemctl", "--user", "show"]
                and "validate-entry-running-tampered-admission.service" in command
            ):
                return completed(
                    command,
                    stdout=(
                        "LoadState=loaded\nActiveState=active\nSubState=running\n"
                        "InvocationID=entry-running-tampered-admission-fixture\n"
                    ),
                )
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )
        self.assertEqual([], report["removed"])
        self.assertEqual(1, len(report["retained"]))
        self.assertTrue(report["retained"][0]["blocks_entry"])
        self.assertEqual("unit-running", report["retained"][0]["reason"])
        command_count = len(self.fake.commands)

        rc, _output, error = self.invoke(run=run)

        self.assertEqual(0, rc, error)
        self.assertTrue(frozen.is_dir())
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        entry_commands = self.fake.commands[command_count:]
        self.assertFalse(
            any(
                wrkslots_action(command, "recover-ownerless-validate-batch")
                for command in entry_commands
            )
        )
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in entry_commands)
        )

    def test_entry_cleanup_allows_typed_historical_frozen_retention(self) -> None:
        frozen = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-entry-historical"
        )
        (frozen / ".git").mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-entry-historical.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-entry-historical.service",
                "checkout": str(frozen),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "admission": start_unit.FROZEN_RESULT_ADMISSION,
                "repo": "rrnewton/hermit",
                "state": "unknown",
                "result": "unknown",
                "detail": "historical schema 3 has no removal proof",
            },
        )
        reason = (
            "historical schema 3 frozen validation has no removal proof; "
            "the inactive checkout remains retained"
        )
        self.fake.ownerless_batch_retained[str(frozen)] = reason
        self.fake.ownerless_batch_nonblocking.add(str(frozen))

        sweep = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        self.assertEqual([], sweep["removed"])
        self.assertEqual(1, len(sweep["retained"]))
        self.assertFalse(sweep["retained"][0]["blocks_entry"])
        command_count = len(self.fake.commands)

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(frozen.is_dir())
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=1", error)
        entry_commands = self.fake.commands[command_count:]
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in entry_commands
                for action in (
                    "classify-ownerless-validate-batch",
                    "recover-ownerless-validate-batch",
                )
            )
        )
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_ignores_disjoint_frozen_checkout_named_by_multiple_records(
        self,
    ) -> None:
        frozen = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-entry-mixed-records"
        )
        (frozen / ".git").mkdir(parents=True)
        common = {
            "schema_version": 1,
            "checkout": str(frozen),
            "source_checkout": str(self.checkout),
            "temporary_checkout": True,
            "admission": start_unit.FROZEN_RESULT_ADMISSION,
            "kind": "frozen-validate",
            "validation_kind": "frozen-validate",
            "repo": "rrnewton/hermit",
        }
        start_unit.run_registry.write_record(
            self.root
            / "ignored/validate/runs/validate-a-entry-historical.json",
            {
                **common,
                "unit": "validate-a-entry-historical.service",
                "state": "unknown",
                "result": "unknown",
                "result_source": "historical-validation-service-result",
                "service_result_schema": 3,
                "detail": "historical schema 3 has no current authority",
            },
        )
        start_unit.run_registry.write_record(
            self.root / "ignored/validate/runs/validate-z-entry-current.json",
            {
                **common,
                "unit": "validate-z-entry-current.service",
                "state": "completed",
                "result": "success",
                "result_source": "validation-service-result",
                "service_result_schema": 5,
                "final_validate_status": "PASSED",
                "exit_code": 0,
            },
        )

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(frozen.is_dir())
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertFalse(
            any(
                wrkslots_action(command, "recover-ownerless-validate-batch")
                for command in self.fake.commands
            )
        )
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_ignores_disjoint_cleanup_reason_text(self) -> None:
        frozen = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-entry-reason-only"
        )
        (frozen / ".git").mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-entry-reason-only.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-entry-reason-only.service",
                "checkout": str(frozen),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "admission": start_unit.FROZEN_RESULT_ADMISSION,
                "repo": "rrnewton/hermit",
                "state": "unknown",
                "result": "unknown",
                "detail": "historical schema 3 has no removal proof",
            },
        )
        self.fake.ownerless_batch_retained[str(frozen)] = "unit-running"

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(frozen.is_dir())
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_ignores_disjoint_frozen_checkout_without_cleanup_proof(self) -> None:
        frozen = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-entry-current-no-proof"
        )
        (frozen / ".git").mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-entry-current.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-entry-current.service",
                "checkout": str(frozen),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "admission": start_unit.FROZEN_RESULT_ADMISSION,
                "repo": "rrnewton/hermit",
                "state": "completed",
                "result": "success",
                "service_result_schema": 5,
                "final_validate_status": "PASSED",
                "exit_code": 0,
            },
        )
        reason = (
            "frozen validation has no typed removal proof; the checkout remains "
            "retained and blocks entry"
        )
        self.fake.ownerless_batch_retained[str(frozen)] = reason

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(frozen.is_dir())
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_cleanup_preserves_current_incomplete_frozen_result(self) -> None:
        frozen_parent = start_unit.frozen_checkout_parent(self.root)
        self.addCleanup(shutil.rmtree, frozen_parent, True)
        frozen = frozen_parent / "validate-fresh-entry-current-incomplete"
        (frozen / ".git").mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-entry-incomplete.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-entry-incomplete.service",
                "checkout": str(frozen),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "admission": start_unit.FROZEN_RESULT_ADMISSION,
                "repo": "rrnewton/hermit",
                "state": "completed",
                "result": "no-result",
                "result_source": "validation-service-result",
                "service_result_schema": 5,
                "final_validate_status": "COULD_NOT_RUN",
                "exit_code": 75,
                "executed_nodes": 0,
                "executed_tests": None,
                "passed_tests": None,
                "selection_mode": None,
                "scorecard_writeback": None,
                "qualifying_receipt": False,
            },
        )
        before = record.read_bytes()
        reason = (
            "current incomplete frozen validation is exact and inactive; "
            "the checkout and no-result remain retained"
        )
        self.fake.ownerless_batch_output = json.dumps(
            {
                "schema": 1,
                "requested": 1,
                "process_censuses": 1,
                "create_journals": [],
                "classifications": [
                    {
                        "blocks_entry": False,
                        "checkout": str(frozen),
                        "reason": reason,
                        "state": "current-incomplete-retained",
                    }
                ],
            }
        )

        start_unit.require_validate_entry_cleanup(
            self.root,
            prospective_slot="validate-fresh-next",
            prospective_checkout=frozen_parent / "validate-fresh-next",
            run=self.fake,
            tool_root=self.root,
        )

        self.assertEqual(before, record.read_bytes())
        self.assertTrue(frozen.is_dir())
        self.assertEqual([], self.fake.removed)
        commands = self.fake.commands
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in commands
                for action in (
                    "classify-ownerless-validate-batch",
                    "recover",
                    "recover-ownerless-validate-batch",
                    "remove",
                )
            )
        )
        payload = json.loads(self.fake.ownerless_batch_output)
        for field, replacement in (("blocks_entry", True), ("blocks_entry", 0), ("process_censuses", 0)):
            with self.subTest(field=field, replacement=replacement):
                altered = json.loads(json.dumps(payload))
                target = altered if field == "process_censuses" else altered["classifications"][0]
                target[field] = replacement
                self.fake.ownerless_batch_output = json.dumps(altered)
                outcomes = start_unit.remove_ownerless_checkouts_batch(
                    self.root, [(frozen, record, self.checkout)],
                    run=self.fake, tool_root=self.root, frozen=True, classify_only=True,
                )
                self.assertTrue(outcomes[str(frozen)].blocks_entry)
                self.assertIn("invalid ownerless batch report", outcomes[str(frozen)].reason)
                self.assertEqual(before, record.read_bytes())
                self.assertTrue(frozen.is_dir())

    def test_entry_cleanup_uses_typed_unit_state_when_reason_text_changes(self) -> None:
        running, _record = self.managed_cleanup_record(
            "entry-running-rendered-differently", state="running"
        )
        observation = start_unit.CleanupUnitObservation(
            None,
            start_unit.CleanupUnitState.RUNNING,
            "rendered active-unit explanation changed",
        )

        with mock.patch.object(
            start_unit, "terminal_cleanup_unit", return_value=observation
        ):
            rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(running.exists())
        self.assertIn("ENTRY-AUTHORITY state=ready", error)
        self.assertIn("run-handles=1", error)
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_in_place_launch_does_not_run_disposable_checkout_cleanup(self) -> None:
        stale, _record = self.managed_cleanup_record("in-place-unrelated")
        self.fake.validate_batch_retained[stale.name] = "provider retained"

        rc, _output, error = self.invoke(["--in-place"])

        self.assertEqual(0, rc, error)
        self.assertTrue(stale.exists())
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_detached_source_does_not_infer_main_branch(self) -> None:
        self.fake.source_branch = None

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertNotIn("branch", record)
        symbolic_ref = next(
            command
            for command in self.fake.commands
            if command[:4] == ["git", "-C", str(self.checkout), "symbolic-ref"]
        )
        self.assertEqual(
            ["symbolic-ref", "--quiet", "--short", "HEAD"], symbolic_ref[3:]
        )

    def test_current_target_records_and_consumes_framework_result(self) -> None:
        self.write_validation_service_schema(start_unit.service_result.SCHEMA_VERSION)

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        service_result_env = next(
            value
            for value in systemd
            if value.startswith("VALIDATE_SERVICE_RESULT_PATH=")
        )
        record_path = self.root / "ignored/validate/runs/validate-test.json"
        self.assertEqual(
            str(start_unit.service_result.result_path(record_path)),
            service_result_env.removeprefix("VALIDATE_SERVICE_RESULT_PATH="),
        )
        record = start_unit.run_registry.read_record(record_path)
        self.assertEqual(
            start_unit.service_result.SCHEMA_VERSION,
            record["service_result_schema"],
        )
        self.assertEqual("full", record["selection_mode"])
        self.assertEqual("validation-service-result", record["result_source"])
        self.assertEqual(55, record["executed_nodes"])
        self.assertEqual(862, record["executed_tests"])
        self.assertEqual(862, record["passed_tests"])
        self.assertEqual({"status": "completed"}, record["scorecard_writeback"])

    def test_current_target_refuses_result_missing_passed_count(self) -> None:
        self.write_validation_service_schema(
            start_unit.service_result.SCHEMA_VERSION
        )
        self.fake.include_passed_tests = False
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(75, rc, error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("unknown", record["state"])
        self.assertIsNone(record["passed_tests"])
        self.assertIn("schema 5 expected", record["detail"])

    def test_current_target_refuses_mismatched_passed_count(self) -> None:
        self.write_validation_service_schema(
            start_unit.service_result.SCHEMA_VERSION
        )
        self.fake.passed_tests = 861
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(75, rc, error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("unknown", record["state"])
        self.assertIsNone(record["passed_tests"])
        self.assertIn("requires passed_tests == executed_tests", record["detail"])

    def assert_historical_service_result_keeps_passed_unknown(
        self, version: int
    ) -> None:
        self.write_validation_service_schema(version)
        self.fake.service_result_schema = version
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(75, rc, error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("unknown", record["state"])
        self.assertEqual(version, record["service_result_schema"])
        self.assertIsNone(record["passed_tests"])
        self.assertIn(f"historical schema {version}", record["detail"])

    def test_schema_one_result_keeps_passed_count_unknown(self) -> None:
        self.assert_historical_service_result_keeps_passed_unknown(1)

    def test_schema_two_result_keeps_passed_count_unknown(self) -> None:
        self.assert_historical_service_result_keeps_passed_unknown(2)

    def test_schema_three_result_keeps_passed_count_unknown(self) -> None:
        self.assert_historical_service_result_keeps_passed_unknown(3)

    def test_current_target_missing_framework_result_does_not_fall_back_to_log(self) -> None:
        self.write_validation_service_schema(start_unit.service_result.SCHEMA_VERSION)
        self.fake.write_service_result = False
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(75, rc, error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("unknown", record["state"])
        self.assertEqual("validation-service-result", record["result_source"])
        self.assertIn("validation-service-result-read", record["detail"])
        self.assertFalse(
            any(
                len(command) > 1 and command[1] == "publish-commit-status"
                for command in self.fake.commands
            )
        )

    def test_current_target_schema_mutation_refuses_before_launch(self) -> None:
        schema_path = self.checkout / start_unit.service_result.SCHEMA_RELATIVE_PATH
        self.write_validation_service_schema(start_unit.service_result.SCHEMA_VERSION)
        schema = json.loads(schema_path.read_text())
        schema["fields"].append("future")
        schema_path.write_text(json.dumps(schema))

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("validation-service-result-schema-fields", error)
        self.assertIn("future", error)
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            )
        )

    def test_current_target_schema_extra_top_level_field_refuses_before_launch(
        self,
    ) -> None:
        schema_path = self.checkout / start_unit.service_result.SCHEMA_RELATIVE_PATH
        self.write_validation_service_schema(start_unit.service_result.SCHEMA_VERSION)
        schema = json.loads(schema_path.read_text())
        schema["future"] = True
        schema_path.write_text(json.dumps(schema))

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("validation-service-result-schema-fields", error)
        self.assertIn("future", error)
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            )
        )

    def test_completed_validate_publishes_ledger_after_receipt_before_cleanup(self) -> None:
        self.plant_per_cell_results()
        before = self.worktree_bytes(self.checkout)
        rc, output, error = self.invoke()
        self.assertEqual(0, rc, error)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.assertIn("CELL LEDGER PUBLISHED", error)
        receipt_index = next(i for i, c in enumerate(self.fake.commands)
                             if c[0].endswith("ci-hub") and c[1:3] == ["validate-status", "--sha"])
        ledger_index = next(i for i, c in enumerate(self.fake.commands)
                            if len(c) > 1 and c[1].endswith("ci-hub/series/mirror.py"))
        status_index = next(i for i, c in enumerate(self.fake.commands)
                            if len(c) > 1 and c[1] == "publish-commit-status")
        cleanup_index = next(i for i, c in enumerate(self.fake.commands) if wrkslots_action(c, "remove"))
        self.assertLess(receipt_index, ledger_index)
        self.assertLess(ledger_index, status_index)
        self.assertLess(status_index, cleanup_index)
        self.assertFalse(any(c[0].endswith("scorecard.rs") for c in self.fake.commands))

    def test_green_validate_publishes_exact_sha_from_canonical_state_root_once(self) -> None:
        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        publications = [
            (index, command)
            for index, command in enumerate(self.fake.commands)
            if len(command) > 1 and command[1] == "publish-commit-status"
        ]
        self.assertEqual(1, len(publications))
        index, command = publications[0]
        self.assertEqual(SHA, command[command.index("--sha") + 1])
        self.assertEqual("rrnewton/hermit", command[command.index("--repo") + 1])
        self.assertEqual(self.root, self.fake.command_cwds[index][1])
        environment = self.fake.command_envs[index]
        self.assertIsNotNone(environment)
        assert environment is not None
        self.assertEqual(str(self.root), environment["DEV_HERMIT_PARENT"])
        self.assertEqual(str(self.root), environment["DEV_HERMIT_TOOL_ROOT"])
        self.assertEqual(sys.executable, environment["FLEET_REAL_GH"])
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        publication = record["commit_status_publication"]
        self.assertEqual(sys.executable, record["github_executable"])
        self.assertEqual("published", publication["state"])
        self.assertEqual(SHA, publication["sha"])

    def test_missing_github_route_refuses_before_checkout_or_service_creation(self) -> None:
        environment = dict(self.environment)
        environment.pop("FLEET_REAL_GH")
        rc, _output, error = self.invoke(environment=environment)
        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("FLEET_REAL_GH", error)
        self.assertFalse(any(command[0] == "systemd-run" or wrkslots_action(command, "create")
                             for command in self.fake.commands))

    def test_non_green_validate_records_skip_without_invoking_publisher(self) -> None:
        self.fake.canonical_verdict = "FAILED"
        self.fake.canonical_status_rc = start_unit.EXIT_FAILED

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_FAILED, rc, error)
        self.assertFalse(
            any(
                len(command) > 1 and command[1] == "publish-commit-status"
                for command in self.fake.commands
            )
        )
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        publication = record["commit_status_publication"]
        self.assertEqual("not-attempted", publication["state"])
        self.assertEqual("canonical verdict is FAILED", publication["reason"])

    def test_publisher_failure_is_recorded_without_changing_green_verdict(self) -> None:
        self.fake.commit_status_rc = 1
        self.fake.commit_status_stderr = "network unavailable"

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc)
        self.assertIn("COMMIT STATUS NOT PUBLISHED", error)
        self.assertIn("canonical validation verdict is unchanged", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        publication = record["commit_status_publication"]
        self.assertEqual("failed", publication["state"])
        self.assertEqual("network unavailable", publication["detail"])

    def test_publisher_launch_failure_is_recorded_without_changing_green_verdict(
        self,
    ) -> None:
        self.fake.commit_status_launch_error = OSError("publisher executable unavailable")

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc)
        self.assertIn("COMMIT STATUS NOT PUBLISHED", error)
        self.assertIn("canonical validation verdict is unchanged", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        publication = record["commit_status_publication"]
        self.assertEqual("failed", publication["state"])
        self.assertEqual(
            "cannot launch publisher: publisher executable unavailable",
            publication["detail"],
        )

    def test_ledger_mirror_success_is_recorded(self) -> None:
        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertIn("CELL LEDGER PUBLISHED", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(
            {
                "state": "published",
                "commit": "d" * 40,
                "source_commit": "e" * 40,
                "source_tree": "f" * 40,
                "rows": 17,
            },
            {
                key: value
                for key, value in record["ledger_mirror"].items()
                if key in {"state", "commit", "source_commit", "source_tree", "rows"}
            },
        )
        self.assertTrue(start_unit.cell_ledger_published(record))
        self.assertFalse(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))
        publication = next(command for command in self.fake.commands
                           if len(command) > 1 and command[1].endswith("ci-hub/series/mirror.py"))
        self.assertIn("--results", publication)
        self.assertEqual("validate-test", publication[publication.index("--finalized-run-id") + 1])

    def test_ledger_mirror_failure_is_loud_without_changing_green_verdict(self) -> None:
        self.fake.ledger_mirror_rc = 1
        self.fake.ledger_mirror_stderr = "remote push refused"

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc)
        self.assertIn("CELL LEDGER PUBLICATION INCOMPLETE", error)
        self.assertIn("remote push refused", error)
        self.assertIn("canonical validation verdict is unchanged", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("failed", record["ledger_mirror"]["state"])
        self.assertIn("remote push refused", record["ledger_mirror"]["detail"])

    def test_ledger_mirror_real_subprocess_records_success_after_red_validate(self) -> None:
        self._ledger_mirror_real_subprocess(exit_code=0)

    def test_ledger_mirror_real_subprocess_records_failure_after_red_validate(self) -> None:
        self._ledger_mirror_real_subprocess(exit_code=7)

    def _ledger_mirror_real_subprocess(self, *, exit_code: int) -> None:
        # A synthetic terminal record isolates bookkeeping from validation
        # admission. The mirror is a REAL Python child reached through the
        # production finalize_ledger_mirror -> run_command boundary.
        mirror = self.root / "ci-hub/series/mirror.py"
        mirror.parent.mkdir(parents=True)
        report = publication_report(SHA)
        mirror.write_text(
            "import json, sys\n"
            f"print(json.dumps({report!r}))\n"
            "print('synthetic mirror diagnostic', file=sys.stderr)\n"
            f"sys.exit({exit_code})\n"
        )
        record_path = self.root / "mirror-fixture.json"
        record = {
            "schema_version": run_registry.SCHEMA_VERSION,
            "producer": "synthetic-mirror-boundary-control",
            "repo": "rrnewton/hermit", "checkout": str(self.checkout),
            "target": SHA, "canonical_verdict": "FAILED",
            "wrapper_exit_code": start_unit.EXIT_FAILED,
        }
        run_registry.write_record(record_path, record)
        calls = []

        def run(command, **kwargs):
            self.assertEqual(str(mirror), command[1])
            calls.append((command, kwargs))
            return start_unit.run_command(command, **kwargs)

        errors = io.StringIO()
        with (contextlib.redirect_stderr(errors),
              mock.patch.object(start_unit.time, "monotonic_ns", return_value=1_000_000_000_000)):
            result = start_unit.finalize_ledger_mirror(
                self.root, self.root, record_path, record=record, run=run,
            )

        error = errors.getvalue()
        self.assertEqual(1, len(calls))
        self.assertIs(False, calls[0][1]["check"])
        self.assertEqual(1800, calls[0][1]["timeout"])
        record = start_unit.run_registry.read_record(record_path)
        self.assertEqual(record, result)
        self.assertEqual("FAILED", record["canonical_verdict"])
        self.assertEqual(start_unit.EXIT_FAILED, record["wrapper_exit_code"])
        if exit_code == 0:
            self.assertEqual("published", record["ledger_mirror"]["state"])
            self.assertEqual(17, record["ledger_mirror"]["rows"])
            self.assertIn("CELL LEDGER PUBLISHED", error)
        else:
            self.assertEqual("failed", record["ledger_mirror"]["state"])
            self.assertIn("exit 7", record["ledger_mirror"]["detail"])
            self.assertIn("synthetic mirror diagnostic", record["ledger_mirror"]["detail"])
            self.assertIn("CELL LEDGER PUBLICATION INCOMPLETE", error)

    def test_mirror_deadline_uses_actual_remaining_stop_post_budget(self) -> None:
        start = 10_000_000_000
        def run(command, **kwargs):
            self.assertEqual(["systemctl", "--user", "show", "validate-test.service",
                              "-p", "ActiveExitTimestampMonotonic", "-p", "TimeoutStopUSec"], command)
            return subprocess.CompletedProcess(command, 0,
                "ActiveExitTimestampMonotonic=5000000\nTimeoutStopUSec=10min\n", "")
        with mock.patch.object(Path, "read_text", return_value="0::/user.slice/validate-test.service\n"):
            deadline = start_unit._retirement_publication_deadline(
                "validate-test", run=run, started_ns=start)
        self.assertEqual(605_000_000_000, deadline)
        self.assertEqual(595_000_000_000, deadline - start)

    def test_stop_post_deadline_refuses_unbound_unit_timing(self) -> None:
        for values in ("", "ActiveExitTimestampMonotonic=0\nTimeoutStopUSec=10min\n",
                       "ActiveExitTimestampMonotonic=5000000\nTimeoutStopUSec=1h\n"):
            with self.subTest(values=values), mock.patch.object(
                Path, "read_text", return_value="0::/user.slice/validate-test.service\n"
            ), self.assertRaisesRegex(ValueError, "active stop-post deadline"):
                start_unit._retirement_publication_deadline(
                    "validate-test", started_ns=10_000_000_000,
                    run=lambda command, **kwargs: subprocess.CompletedProcess(command, 0, values, ""))

    def test_successful_publication_is_not_repeated_on_reentry(self) -> None:
        rc, _output, error = self.invoke()
        self.assertEqual(0, rc, error)
        record_path = self.root / "ignored/validate/runs/validate-test.json"
        before = sum(
            len(command) > 1 and command[1] == "publish-commit-status"
            for command in self.fake.commands
        )

        start_unit.finalize_commit_status_publication(
            self.root,
            self.root,
            record_path,
            record=start_unit.run_registry.read_record(record_path),
            target=SHA,
            repo="rrnewton/hermit",
            canonical_verdict="VALIDATED",
            canonical_exit=0,
            scorecard_updated=True,
            run=self.fake,
        )

        after = sum(
            len(command) > 1 and command[1] == "publish-commit-status"
            for command in self.fake.commands
        )
        self.assertEqual(before, after)

    def test_completion_without_local_cell_files_still_calls_ledger_publisher(self) -> None:
        rc, output, error = self.invoke()
        self.assertEqual(0, rc, error)
        self.assertIn("CELL LEDGER PUBLISHED", error)
        self.assertFalse(any(c[0].endswith("scorecard.rs") for c in self.fake.commands))
        self.assertTrue(any(len(c) > 1 and c[1].endswith("ci-hub/series/mirror.py") for c in self.fake.commands))

    def test_retired_writeback_helper_never_invokes_a_hermit_writer(self) -> None:
        results = self.plant_per_cell_results()
        self.fake.source_head = "b" * 40
        out, error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(error):
            updated = start_unit.write_scorecard_from_results(
                self.checkout, SHA, results, run=self.fake, json_output=False)
        self.assertIsNone(updated)
        self.assertIn("local scorecard writeback is retired", error.getvalue())
        self.assertIn("canonical validation verdict is unchanged", error.getvalue())
        self.assertEqual([], self.fake.commands)

    def test_pending_hermit_scorecard_edits_are_preserved_by_publication(self) -> None:
        self.plant_per_cell_results()
        (self.checkout / "SCORECARD.md").write_text("pending review\n")
        before = self.worktree_bytes(self.checkout)
        rc, _output, error = self.invoke()
        self.assertEqual(0, rc, error)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.assertIn("CELL LEDGER PUBLISHED", error)
        self.assertFalse(any(c[0].endswith("scorecard.rs") for c in self.fake.commands))

    def test_red_validate_still_publishes_ledger_without_rewriting_verdict(self) -> None:
        self.plant_per_cell_results()
        self.fake.canonical_verdict = "FAILED"
        self.fake.canonical_status_rc = start_unit.EXIT_FAILED
        before = self.worktree_bytes(self.checkout)
        rc, output, error = self.invoke()
        self.assertEqual(start_unit.EXIT_FAILED, rc, error)
        self.assertIn("verdict=FAILED exit=3", output)
        self.assertIn("CELL LEDGER PUBLISHED", error)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.assertFalse(any(c[0].endswith("scorecard.rs") for c in self.fake.commands))

    def test_derived_failure_preserves_green_verdict_and_retains_inputs(self) -> None:
        results = self.plant_per_cell_results()
        self.fake.scorecard_rc = 2
        self.fake.scorecard_stderr = "observe-results refuses unrelated tracked changes\n"
        rc, output, error = self.invoke()
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("RECEIPT-CANONICAL", output)
        self.assertIn("verdict=VALIDATED exit=0", output)
        self.assertIn("refuses unrelated tracked changes", error)
        self.assertIn("CELL LEDGER PUBLICATION INCOMPLETE", error)
        self.assertIn("canonical validation verdict is unchanged", error)
        self.assertFalse(any(len(c) > 1 and c[1] == "publish-commit-status" for c in self.fake.commands))
        self.assertEqual([], self.fake.removed)
        self.assertTrue(results.exists())
        record = run_registry.read_record(self.root / "ignored/validate/runs/validate-test.json")
        self.assertEqual("derived-failed", record["ledger_mirror"]["state"])
        self.assertEqual("e" * 40, record["ledger_mirror"]["series_commit"])
        self.assertFalse(start_unit.cell_ledger_published(record))
        self.assertEqual("scorecard write-back did not complete", record["commit_status_publication"]["reason"])

    def test_derived_failure_records_the_published_proof_separately(self) -> None:
        self.plant_per_cell_results()
        self.fake.scorecard_rc = 2
        self.fake.scorecard_stderr = "renderer exploded\n"
        proof = {"state": "published", "commit": "d" * 40, "hermit_commit": SHA,
                 "tool_hermit_commit": "9" * 40,
                 "files": {"scorecard/finalized/runs/" + "3" * 64 + ".json": "4" * 64,
                           "scorecard/finalized/objects/sha256/" + "5" * 64: "5" * 64}}
        self.fake.ledger_mirror_update = {"portable_proof": proof}
        rc, _output, error = self.invoke()
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn(f"FINALIZED PROOF PUBLISHED: commit={'d' * 40}", error)
        self.assertIn("CELL LEDGER PUBLICATION INCOMPLETE: renderer exploded", error)
        record = run_registry.read_record(self.root / "ignored/validate/runs/validate-test.json")
        self.assertEqual("derived-failed", record["ledger_mirror"]["state"])
        self.assertEqual({"state": "failed", "error": "renderer exploded\n"}, record["ledger_mirror"]["scorecard"])
        self.assertEqual(proof, record["ledger_mirror"]["portable_proof"])
        self.assertFalse(start_unit.cell_ledger_published(record))
        self.assertEqual([], self.fake.removed)
        self.assertEqual("scorecard write-back did not complete", record["commit_status_publication"]["reason"])

    def test_superseded_scorecard_completes_the_ledger_publication(self) -> None:
        self.fake.ledger_mirror_update = {"scorecard": {
            "state": "superseded", "hermit_commit": SHA, "current_hermit_commit": "b" * 40,
            "detail": "published scorecard head bbbb is newer; latest.json and SCORECARD.md were left unchanged"}}
        rc, _output, error = self.invoke()
        self.assertEqual(0, rc, error)
        self.assertIn("SCORECARD SUPERSEDED: published scorecard head bbbb is newer", error)
        self.assertIn("CELL LEDGER PUBLISHED", error)
        record = run_registry.read_record(self.root / "ignored/validate/runs/validate-test.json")
        self.assertEqual("superseded", record["ledger_mirror"]["scorecard"]["state"])
        self.assertTrue(start_unit.cell_ledger_published(record))
        self.assertTrue(any(len(c) > 1 and c[1] == "publish-commit-status" for c in self.fake.commands))

    def test_cell_ledger_published_accepts_only_an_exact_superseded_scorecard(self) -> None:
        base = {"target": SHA, "ledger_mirror": {
            **publication_report(SHA), "repository": start_unit.CELL_LEDGER_REPOSITORY}}
        superseded = {"state": "superseded", "hermit_commit": SHA, "current_hermit_commit": "b" * 40,
                      "detail": "newer head"}
        self.assertTrue(start_unit.cell_ledger_published(base))
        self.assertTrue(start_unit.cell_ledger_published(
            {**base, "ledger_mirror": {**base["ledger_mirror"], "scorecard": superseded}}))
        for label, scorecard in (
            ("names itself as newer", {**superseded, "current_hermit_commit": SHA}),
            ("newer head not a SHA", {**superseded, "current_hermit_commit": "newer"}),
            ("newer head absent", {key: value for key, value in superseded.items()
                                   if key != "current_hermit_commit"}),
            ("other commit", {**superseded, "hermit_commit": "c" * 40}),
            ("failed", {"state": "failed", "error": "x", "hermit_commit": SHA}),
        ):
            with self.subTest(label):
                self.assertFalse(start_unit.cell_ledger_published(
                    {**base, "ledger_mirror": {**base["ledger_mirror"], "scorecard": scorecard}}))
        self.assertFalse(start_unit.cell_ledger_published(
            {**base, "ledger_mirror": {**base["ledger_mirror"], "state": "derived-failed",
                                       "scorecard": superseded}}))

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
        self.assertIn(f"DEV_HERMIT_TOOL_ROOT={linked}", systemd)
        self.assertNotIn(f"DEV_HERMIT_TOOL_ROOT={self.root}", systemd)

    def test_fd_backed_tool_root_reenters_exact_installed_bootstrap(self) -> None:
        home = self.root / "home"
        bootstrap = home / ".local/libexec/dev-hermit/operational-tool"
        bootstrap.parent.mkdir(parents=True)
        bootstrap.write_text("#!/bin/sh\nexit 0\n")
        bootstrap.chmod(0o555)
        cache = self.root / "tool-cache"
        target_root = cache / "trees" / SHA
        target_root.mkdir(parents=True)
        make_tree_read_only(target_root)
        authority = self.authority_fixture(bootstrap, target_root=target_root)
        retained = authority.tool_root
        environment = dict(
            self.environment,
            **authority.environment(),
            HOME=str(home),
            DEV_HERMIT_TOOL_ROOT=str(retained),
            DEV_HERMIT_PARENT=str(self.root),
            DEV_HERMIT_OPERATIONAL_TOOL=str(bootstrap),
            DEV_HERMIT_TOOL_CACHE=str(cache),
        )
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
                    "owner-checkout-watch",
                    "--target",
                    SHA,
                    "--unit",
                    "validate-owner-fd-root",
                    "--log",
                    str(self.root / "owner-fd.log"),
                    "--dry-run",
                    "--json",
                    "--",
                    "full",
                ],
                run=self.fake,
                environment=environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(0, rc, err.getvalue())
        report = json.loads(out.getvalue())
        command = report["command"]
        bootstrap_digest = hashlib.sha256(bootstrap.read_bytes()).hexdigest()
        bootstrap_index = command.index(start_unit.PINNED_BOOTSTRAP_EXEC) - 2
        self.assertEqual(
            [
                sys.executable,
                "-c",
                start_unit.PINNED_BOOTSTRAP_EXEC,
                str(bootstrap),
                bootstrap_digest,
                "--run-current-tool",
                "--state-root",
                str(self.root),
                "--state-identity",
                f"{authority.record['state_dev']}:{authority.record['state_ino']}",
                "--target-identity",
                f"{authority.record['target_dev']}:{authority.record['target_ino']}",
                "--cache-root",
                str(cache),
                "--tool-parent-sha",
                SHA,
                "--tool-relative",
                "ci-hub/ci-hub",
                "--",
                "validate-lock",
                "run",
            ],
            command[bootstrap_index : bootstrap_index + 21],
        )
        self.assertNotIn(str(retained / "ci-hub/ci-hub"), command)
        self.assertNotIn(f"DEV_HERMIT_TOOL_ROOT={retained}", command)
        self.assertNotIn(str(retained), " ".join(command))
        self.assertIn(f"DEV_HERMIT_PARENT={self.root}", command)
        self.assertNotIn(f"DEV_HERMIT_TOOL_ROOT={self.root}", command)

    def test_fd_authority_accepts_sealed_same_holder_identity_and_pins(self) -> None:
        import immutable_tool_authority

        self.assertIs(
            immutable_tool_authority.read_immutable_tool_authority,
            start_unit.read_immutable_tool_authority,
        )
        bootstrap = self.root / "authority-bootstrap"
        bootstrap.write_text("#!/bin/sh\nexit 0\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap)

        observed = start_unit.read_immutable_tool_authority(
            authority.tool_root, self.root, authority.environment()
        )

        self.assertEqual(SHA, observed.parent_sha)
        self.assertEqual(HERMIT_SHA, observed.hermit_sha)
        self.assertEqual(AGENT_UTILS_SHA, observed.agent_utils_sha)
        self.assertEqual(authority.record["content_sha256"], observed.content_sha256)
        self.assertEqual(authority.target_fd, observed.target_fd)
        self.assertEqual(authority.target_root, observed.target_root)

    def test_fd_authority_incomplete_record_is_refused(self) -> None:
        bootstrap = self.root / "incomplete-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(
            bootstrap, remove={"agent_utils_sha"}
        )

        with self.assertRaisesRegex(ValueError, "fields are incomplete"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_unsealed_record_is_refused(self) -> None:
        bootstrap = self.root / "unsealed-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap, sealed=False)

        with self.assertRaisesRegex(ValueError, "completely sealed"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_copied_record_is_refused(self) -> None:
        bootstrap = self.root / "copied-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap)
        copied_fd = os.memfd_create(
            "copied-dev-hermit-tool-authority",
            os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING,
        )
        self.addCleanup(os.close, copied_fd)
        os.write(copied_fd, authority.authority_path.read_bytes())
        os.fchmod(copied_fd, 0o400)
        fcntl.fcntl(
            copied_fd,
            fcntl.F_ADD_SEALS,
            fcntl.F_SEAL_SEAL
            | fcntl.F_SEAL_SHRINK
            | fcntl.F_SEAL_GROW
            | fcntl.F_SEAL_WRITE,
        )
        environment = authority.environment()
        environment[start_unit.TOOL_AUTHORITY_ENV] = (
            f"/proc/{os.getpid()}/fd/{copied_fd}"
        )

        with self.assertRaisesRegex(ValueError, "does not name the supplied descriptors"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, environment
            )

    def test_fd_authority_wrong_root_descriptor_is_refused(self) -> None:
        bootstrap = self.root / "wrong-root-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap)
        wrong_root = self.root / "wrong-authority-root"
        wrong_root.mkdir()
        make_tree_read_only(wrong_root)
        wrong_fd = os.open(
            wrong_root,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
        self.addCleanup(os.close, wrong_fd)

        with self.assertRaisesRegex(ValueError, "supplied descriptors"):
            start_unit.read_immutable_tool_authority(
                Path(f"/proc/{os.getpid()}/fd/{wrong_fd}"),
                self.root,
                authority.environment(),
            )

    def test_fd_authority_wrong_target_descriptor_is_refused(self) -> None:
        bootstrap = self.root / "wrong-target-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        wrong_target = self.root / "wrong-authority-target"
        wrong_target.mkdir()
        make_tree_read_only(wrong_target)
        wrong_target_fd = os.open(
            wrong_target,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
        self.addCleanup(os.close, wrong_target_fd)
        authority = self.authority_fixture(
            bootstrap, update={"target_fd": wrong_target_fd}
        )

        with self.assertRaisesRegex(ValueError, "target descriptor identity"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_recorded_holder_mismatch_is_refused(self) -> None:
        bootstrap = self.root / "wrong-holder-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(
            bootstrap, update={"holder_pid": os.getpid() + 1}
        )

        with self.assertRaisesRegex(ValueError, "supplied descriptors"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_from_different_holder_is_refused(self) -> None:
        bootstrap = self.root / "cross-holder-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap)
        holder = subprocess.Popen(
            ["/bin/sleep", "30"],
            pass_fds=(authority.authority_fd,),
        )
        self.addCleanup(holder.wait)
        self.addCleanup(holder.terminate)
        environment = authority.environment()
        environment[start_unit.TOOL_AUTHORITY_ENV] = (
            f"/proc/{holder.pid}/fd/{authority.authority_fd}"
        )

        with self.assertRaisesRegex(ValueError, "owned by different holders"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, environment
            )

    def test_fd_authority_wrong_state_descriptor_is_refused(self) -> None:
        bootstrap = self.root / "wrong-state-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        wrong_state = self.root / "wrong-state"
        wrong_state.mkdir()
        wrong_state_fd = os.open(
            wrong_state,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
        self.addCleanup(os.close, wrong_state_fd)
        authority = self.authority_fixture(
            bootstrap, update={"state_fd": wrong_state_fd}
        )

        with self.assertRaisesRegex(ValueError, "state-root descriptor identity"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_wrong_recorded_inode_is_refused(self) -> None:
        bootstrap = self.root / "wrong-inode-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(
            bootstrap, update={"root_ino": 0}
        )

        with self.assertRaisesRegex(ValueError, "root descriptor identity"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_wrong_target_inode_is_refused(self) -> None:
        bootstrap = self.root / "wrong-target-inode-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(
            bootstrap, update={"target_ino": 0}
        )

        with self.assertRaisesRegex(ValueError, "target descriptor identity"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_wrong_state_inode_is_refused(self) -> None:
        bootstrap = self.root / "wrong-state-inode-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(
            bootstrap, update={"state_ino": 0}
        )

        with self.assertRaisesRegex(ValueError, "state-root descriptor identity"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_wrong_sha_handoff_is_refused(self) -> None:
        bootstrap = self.root / "wrong-sha-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap)
        environment = authority.environment()
        environment[start_unit.TOOL_PARENT_SHA_ENV] = "d" * 40

        with self.assertRaisesRegex(ValueError, "does not match immutable tool authority"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, environment
            )

    def test_fd_authority_wrong_content_digest_is_refused(self) -> None:
        bootstrap = self.root / "wrong-digest-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(
            bootstrap, update={"content_sha256": "d" * 64}
        )

        with self.assertRaisesRegex(ValueError, "content digest is"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_authority_replaced_state_path_is_refused(self) -> None:
        bootstrap = self.root / "replaced-state-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        state_root = self.root / "replaceable-state"
        state_root.mkdir()
        authority = self.authority_fixture(bootstrap, state_root=state_root)
        retained = self.root / "retained-state"
        state_root.rename(retained)
        state_root.mkdir()

        with self.assertRaisesRegex(ValueError, "pathname no longer names"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, state_root, authority.environment()
            )

    def test_fd_authority_replaced_target_path_is_refused(self) -> None:
        bootstrap = self.root / "replaced-target-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap)
        retained_target = authority.target_root.with_name("retained-target")
        authority.target_root.rename(retained_target)
        authority.target_root.mkdir()
        make_tree_read_only(authority.target_root)

        with self.assertRaisesRegex(ValueError, "target pathname no longer names"):
            start_unit.read_immutable_tool_authority(
                authority.tool_root, self.root, authority.environment()
            )

    def test_fd_backed_tool_root_without_lifetime_handoff_is_refused(self) -> None:
        root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, root_fd)
        retained = Path(f"/proc/{os.getpid()}/fd/{root_fd}")
        environment = dict(
            self.environment,
            DEV_HERMIT_TOOL_ROOT=str(retained),
            DEV_HERMIT_PARENT=str(self.root),
        )
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
                    "owner-checkout-watch",
                    "--target",
                    SHA,
                    "--dry-run",
                    "--",
                    "full",
                ],
                run=self.fake,
                environment=environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn(start_unit.TOOL_AUTHORITY_ENV, err.getvalue())
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )

    def test_fd_backed_authority_without_unit_lifetime_handoff_is_refused(
        self,
    ) -> None:
        bootstrap = self.root / "missing-lifetime-bootstrap"
        bootstrap.write_text("#!/bin/sh\n")
        bootstrap.chmod(0o555)
        authority = self.authority_fixture(bootstrap)
        environment = dict(
            self.environment,
            **authority.environment(),
            DEV_HERMIT_TOOL_ROOT=str(authority.tool_root),
            DEV_HERMIT_PARENT=str(self.root),
        )
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
                    "owner-checkout-watch",
                    "--target",
                    SHA,
                    "--dry-run",
                    "--",
                    "full",
                ],
                run=self.fake,
                environment=environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("lacks the installed-bootstrap", err.getvalue())
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_unit_time_bootstrap_digest_rejects_path_replacement(self) -> None:
        home = self.root / "home"
        bootstrap = home / ".local/libexec/dev-hermit/operational-tool"
        bootstrap.parent.mkdir(parents=True)
        marker = self.root / "replacement-ran"
        bootstrap.write_text("#!/bin/sh\nexit 0\n")
        bootstrap.chmod(0o555)
        cache = self.root / "tool-cache"
        target_root = cache / "trees" / SHA
        target_root.mkdir(parents=True)
        make_tree_read_only(target_root)
        authority = self.authority_fixture(bootstrap, target_root=target_root)
        retained = authority.tool_root
        environment = dict(
            self.environment,
            **authority.environment(),
            HOME=str(home),
            DEV_HERMIT_OPERATIONAL_TOOL=str(bootstrap),
            DEV_HERMIT_TOOL_CACHE=str(cache),
        )
        prefix = start_unit.detached_tool_prefix(retained, self.root, environment)
        self.assertIsNotNone(prefix)
        bootstrap.chmod(0o755)
        bootstrap.write_text(f"#!/bin/sh\nprintf poison > {marker}\n")
        bootstrap.chmod(0o555)

        result = subprocess.run(
            prefix or [],
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(126, result.returncode, result.stdout + result.stderr)
        self.assertIn("digest changed before unit exec", result.stderr)
        self.assertFalse(marker.exists())

    def test_unit_time_state_identity_rejects_path_replacement(self) -> None:
        home = self.root / "state-reentry-home"
        bootstrap = home / ".local/libexec/dev-hermit/operational-tool"
        bootstrap.parent.mkdir(parents=True)
        shutil.copy2(
            parent_main_tests.SOURCE / "scripts/hermit-health-tick.sh",
            bootstrap,
        )
        bootstrap.chmod(0o555)
        cache = self.root / "state-reentry-cache"
        target_root = cache / "trees" / SHA
        target_root.mkdir(parents=True)
        make_tree_read_only(target_root)
        materialize_lock = cache / "materialize.lock"
        materialize_lock.touch(mode=0o600)

        def cache_snapshot() -> list[tuple[object, ...]]:
            snapshot: list[tuple[object, ...]] = []
            paths = [cache, *sorted(cache.rglob("*"))]
            for path in paths:
                metadata = path.lstat()
                payload: bytes | str | None = None
                if stat.S_ISREG(metadata.st_mode):
                    payload = path.read_bytes()
                elif stat.S_ISLNK(metadata.st_mode):
                    payload = os.readlink(path)
                snapshot.append(
                    (
                        str(path.relative_to(cache)),
                        stat.S_IFMT(metadata.st_mode),
                        stat.S_IMODE(metadata.st_mode),
                        metadata.st_dev,
                        metadata.st_ino,
                        metadata.st_nlink,
                        metadata.st_size,
                        payload,
                    )
                )
            return snapshot

        cache_before = cache_snapshot()
        state_root = self.root / "state-reentry-root"
        state_root.mkdir()
        authority = self.authority_fixture(
            bootstrap,
            state_root=state_root,
            target_root=target_root,
        )
        environment = dict(
            os.environ,
            **authority.environment(),
            HOME=str(home),
            DEV_HERMIT_OPERATIONAL_TOOL=str(bootstrap),
            DEV_HERMIT_TOOL_CACHE=str(cache),
        )
        prefix = start_unit.detached_tool_prefix(
            authority.tool_root, state_root, environment
        )
        self.assertIsNotNone(prefix)
        expected_identity = (
            f"{authority.record['state_dev']}:{authority.record['state_ino']}"
        )
        self.assertEqual(
            expected_identity,
            (prefix or [])[((prefix or []).index("--state-identity") + 1)],
        )
        retained_state = self.root / "state-reentry-retained"
        state_root.rename(retained_state)
        state_root.mkdir()

        result = subprocess.run(
            prefix or [],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn(
            "canonical state root identity changed across detached reentry",
            result.stderr,
        )
        self.assertEqual(cache_before, cache_snapshot())

    def test_unit_time_target_identity_rejects_path_replacement(self) -> None:
        home = self.root / "target-reentry-home"
        bootstrap = home / ".local/libexec/dev-hermit/operational-tool"
        bootstrap.parent.mkdir(parents=True)
        shutil.copy2(
            parent_main_tests.SOURCE / "scripts/hermit-health-tick.sh",
            bootstrap,
        )
        bootstrap.chmod(0o555)
        cache = self.root / "target-reentry-cache"
        target_root = cache / "trees" / SHA
        target_root.mkdir(parents=True)
        make_tree_read_only(target_root)
        state_root = self.root / "target-reentry-state"
        state_root.mkdir()
        authority = self.authority_fixture(
            bootstrap,
            state_root=state_root,
            target_root=target_root,
        )
        environment = dict(
            os.environ,
            **authority.environment(),
            HOME=str(home),
            DEV_HERMIT_OPERATIONAL_TOOL=str(bootstrap),
            DEV_HERMIT_TOOL_CACHE=str(cache),
        )
        prefix = start_unit.detached_tool_prefix(
            authority.tool_root, state_root, environment
        )
        self.assertIsNotNone(prefix)
        expected_identity = (
            f"{authority.record['target_dev']}:{authority.record['target_ino']}"
        )
        self.assertEqual(
            expected_identity,
            (prefix or [])[((prefix or []).index("--target-identity") + 1)],
        )
        retained_target = cache / "trees" / "retained-target"
        target_root.rename(retained_target)
        target_root.mkdir()
        make_tree_read_only(target_root)

        result = subprocess.run(
            prefix or [],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn(
            "cached exact-parent target identity changed across detached reentry",
            result.stderr,
        )
        self.assertEqual([], list(cache.glob(".run.*")))

    def test_detached_reentry_refuses_same_target_inode_under_other_cache(self) -> None:
        home = self.root / "rebound-target-home"
        bootstrap = home / ".local/libexec/dev-hermit/operational-tool"
        bootstrap.parent.mkdir(parents=True)
        bootstrap.write_text("#!/bin/sh\nexit 0\n")
        bootstrap.chmod(0o555)
        cache_a = self.root / "target-cache-a"
        target_a = cache_a / "trees" / SHA
        target_a.mkdir(parents=True)
        make_tree_read_only(target_a)
        cache_b = self.root / "target-cache-b"
        (cache_b / "trees").mkdir(parents=True)
        authority = self.authority_fixture(
            bootstrap,
            target_root=target_a,
        )
        environment = dict(
            os.environ,
            **authority.environment(),
            HOME=str(home),
            DEV_HERMIT_OPERATIONAL_TOOL=str(bootstrap),
            DEV_HERMIT_TOOL_CACHE=str(cache_b),
        )
        observed = start_unit.read_immutable_tool_authority(
            authority.tool_root, self.root, environment
        )
        original_identity = (target_a.stat().st_dev, target_a.stat().st_ino)
        target_b = cache_b / "trees" / SHA
        (cache_a / "trees").chmod(0o755)
        (cache_b / "trees").chmod(0o755)
        target_a.chmod(0o755)
        target_a.rename(target_b)
        target_b.chmod(0o555)
        self.assertEqual(
            original_identity,
            (target_b.stat().st_dev, target_b.stat().st_ino),
        )

        with self.assertRaisesRegex(
            ValueError,
            "DEV_HERMIT_TOOL_CACHE does not match the cached target root",
        ):
            start_unit.detached_tool_prefix(
                authority.tool_root,
                self.root,
                environment,
                authority=observed,
            )

    def test_unit_time_state_identity_race_is_refused_by_new_holder(self) -> None:
        operational = parent_main_tests.HealthTickParentSyncTests()
        operational.setUp()
        self.addCleanup(operational.tearDown)
        marker = operational.base / "rebound-state-payload-ran"
        target = operational.advance_remote(
            extra_executables={
                "scripts/state-reentry-probe": (
                    "#!/usr/bin/env bash\n"
                    f"printf poison > {marker}\n"
                )
            }
        )
        bootstrap = operational.installed_bootstrap()
        primed = operational.run_current_tool(
            bootstrap, "scripts/state-reentry-probe"
        )
        self.assertEqual(0, primed.returncode, primed.stdout + primed.stderr)
        marker.unlink()
        cache = Path(operational.env["HERMIT_HEALTH_TOOL_CACHE"]).resolve()
        authority = self.authority_fixture(
            bootstrap,
            state_root=operational.repo,
            target_root=cache / "trees" / target,
            update={
                "parent_sha": target,
                "hermit_sha": operational.hermit_head,
                "agent_utils_sha": operational.agent_utils_head,
            },
        )
        environment = dict(
            operational.env,
            **authority.environment(),
            DEV_HERMIT_TOOL_ROOT=str(authority.tool_root),
            DEV_HERMIT_PARENT=str(operational.repo),
            DEV_HERMIT_OPERATIONAL_TOOL=str(bootstrap),
            DEV_HERMIT_TOOL_CACHE=str(cache),
        )
        prefix = start_unit.detached_tool_prefix(
            authority.tool_root, operational.repo, environment
        )
        self.assertIsNotNone(prefix)
        command = prefix or []
        command[command.index("--tool-relative") + 1] = "scripts/state-reentry-probe"

        ready = operational.base / "state-reentry-race-ready"
        proceed = operational.base / "state-reentry-race-proceed"
        shim_dir = operational.base / "state-reentry-race-shim"
        shim_dir.mkdir()
        real_flock = shutil.which("flock") or "/usr/bin/flock"
        flock = shim_dir / "flock"
        flock.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            ": > \"$STATE_REENTRY_READY\"\n"
            "while [[ ! -e \"$STATE_REENTRY_PROCEED\" ]]; do sleep 0.01; done\n"
            f'exec "{real_flock}" "$@"\n'
        )
        flock.chmod(0o755)
        environment.update(
            PATH=f"{shim_dir}:{environment['PATH']}",
            STATE_REENTRY_READY=str(ready),
            STATE_REENTRY_PROCEED=str(proceed),
        )
        process = subprocess.Popen(
            command,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("reentered bootstrap did not reach the post-check barrier")
                time.sleep(0.01)
            self.assertIsNone(process.poll())
            retained_state = operational.base / "state-reentry-race-retained"
            operational.repo.rename(retained_state)
            operational.repo.mkdir()
            proceed.touch()
            stdout, stderr = process.communicate(timeout=20)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(1, process.returncode, stdout + stderr)
        self.assertIn("did not match the prior immutable authority", stderr)
        self.assertFalse(marker.exists())

    def test_accepted_detached_unit_owns_exact_tool_root_after_waiter_returns(
        self,
    ) -> None:
        operational = parent_main_tests.HealthTickParentSyncTests()
        operational.setUp()
        self.addCleanup(operational.tearDown)
        service = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "while [[ ! -e \"$DEV_HERMIT_PARENT/unit-release\" ]]; do sleep 0.01; done\n"
            "\"$DEV_HERMIT_TOOL_ROOT/ci-hub/validate/preflight_validate.py\"\n"
        )
        preflight = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "git -C \"$DEV_HERMIT_TOOL_ROOT\" rev-parse HEAD > "
            "\"$DEV_HERMIT_PARENT/unit-preflight-head\"\n"
        )
        target = operational.advance_remote(
            extra_executables={
                "ci-hub/ci-hub": service,
                "ci-hub/validate/preflight_validate.py": preflight,
            }
        )
        bootstrap = operational.installed_bootstrap()
        primed = operational.run_current_tool(
            bootstrap, "ci-hub/bin/health-tick"
        )
        self.assertEqual(0, primed.returncode, primed.stdout + primed.stderr)
        shutil.rmtree(operational.base / "publisher")
        newer_parent = operational.advance_remote(update_wrapper=True)
        self.assertNotEqual(target, newer_parent)
        cache = Path(operational.env["HERMIT_HEALTH_TOOL_CACHE"]).resolve()
        cached = cache / "trees" / target
        outer = operational.base / "outer-private-tool"
        shutil.copytree(cached, outer, symlinks=True)
        authority = self.authority_fixture(
            bootstrap,
            root=outer,
            target_root=cached,
            update={
                "parent_sha": target,
                "hermit_sha": operational.hermit_head,
                "agent_utils_sha": operational.agent_utils_head,
            },
        )
        retained = authority.tool_root

        class DetachedRun(FakeRun):
            def __init__(self, checkout: Path) -> None:
                super().__init__(checkout)
                self.unit_process: subprocess.Popen[str] | None = None

            def _run_after_checkout_setup(self, command: list[str]):
                if command[0] == "systemd-run" and "validate-lock" in command:
                    unit_environment = dict(os.environ)
                    for flag, value in zip(command, command[1:]):
                        if flag == "--setenv" and "=" in value:
                            key, _, item = value.partition("=")
                            unit_environment[key] = item
                    executable_index = (
                        command.index(start_unit.PINNED_BOOTSTRAP_EXEC) - 2
                    )
                    output = next(
                        value.removeprefix("StandardOutput=append:")
                        for value in command
                        if value.startswith("StandardOutput=append:")
                    )
                    output_handle = Path(output).open("ab")
                    try:
                        self.unit_process = subprocess.Popen(
                            command[executable_index:],
                            cwd=Path(str(self.fresh)),
                            env=unit_environment,
                            stdin=subprocess.DEVNULL,
                            stdout=output_handle,
                            stderr=subprocess.STDOUT,
                            text=True,
                            start_new_session=True,
                        )
                    finally:
                        output_handle.close()
                    return completed(
                        command,
                        stdout="Running as unit: validate-owner-detached.service\n",
                    )
                if command[:3] == ["systemctl", "--user", "show"]:
                    raise RuntimeError("injected waiter transport failure")
                return super()._run_after_checkout_setup(command)

        detached = DetachedRun(self.checkout)
        environment = dict(
            operational.env,
            **authority.environment(),
            XDG_RUNTIME_DIR=str(self.root),
            DEV_HERMIT_TOOL_ROOT=str(retained),
            DEV_HERMIT_PARENT=str(self.root),
            DEV_HERMIT_OPERATIONAL_TOOL=str(bootstrap),
            DEV_HERMIT_TOOL_CACHE=str(cache),
        )
        out = io.StringIO()
        err = io.StringIO()
        try:
            with (
                mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None),
                mock.patch.object(
                    start_unit,
                    "parent_checkout_head",
                    side_effect=AssertionError("fd authority must not consult Git"),
                ),
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
            ):
                rc = start_unit.main(
                    [
                        "--checkout",
                        str(self.checkout),
                        "--state-root",
                        str(self.root),
                        "--agent",
                        "owner-checkout-watch",
                        "--target",
                        SHA,
                        "--unit",
                        "validate-owner-detached",
                        "--log",
                        str(self.root / "owner-detached.log"),
                        "--",
                        "full",
                    ],
                    run=detached,
                    environment=environment,
                    root=self.root,
                    sleep=lambda _seconds: None,
                )

            self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, err.getvalue())
            self.assertIn("WAIT-INTERRUPTED", err.getvalue())
            self.assertIsNotNone(detached.unit_process)
            record = start_unit.run_registry.read_record(
                self.root
                / "ignored/validate/runs/validate-owner-detached.json"
            )
            self.assertEqual(target, record["parent_checkout_head"])
            authority.close()
            parent_main_tests.run("chmod", "-R", "u+w", str(outer), cwd=self.root)
            shutil.rmtree(outer)
            self.assertFalse(retained.exists(), "the launcher-owned fd must be gone")

            (self.root / "unit-release").touch()
            assert detached.unit_process is not None
            unit_rc = detached.unit_process.wait(timeout=20)
            self.assertEqual(
                0,
                unit_rc,
                (self.root / "owner-detached.log").read_text(),
            )
            self.assertEqual(
                target,
                (self.root / "unit-preflight-head").read_text().strip(),
            )
            operational.assert_private_run_pool_is_reusable()
            run_root = next(cache.glob(".run.*"))
            first_identity = (run_root.stat().st_dev, run_root.stat().st_ino)

            repeated = parent_main_tests.run(
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.root),
                "--cache-root",
                str(cache),
                "--tool-parent-sha",
                target,
                "--tool-relative",
                "ci-hub/validate/preflight_validate.py",
                cwd=operational.home,
                env=operational.env,
            )
            self.assertEqual(0, repeated.returncode, repeated.stdout + repeated.stderr)
            operational.assert_private_run_pool_is_reusable()
            reused = next(cache.glob(".run.*"))
            self.assertEqual(first_identity, (reused.stat().st_dev, reused.stat().st_ino))
        finally:
            if detached.unit_process is not None and detached.unit_process.poll() is None:
                detached.unit_process.kill()
                detached.unit_process.wait(timeout=5)

    def test_relative_explicit_tool_root_is_refused_before_checkout_work(self) -> None:
        environment = dict(
            self.environment,
            DEV_HERMIT_TOOL_ROOT="relative/tool-root",
            DEV_HERMIT_PARENT=str(self.root),
        )
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                [
                    "--checkout",
                    str(self.checkout),
                    "--agent",
                    "owner-checkout-watch",
                    "--target",
                    SHA,
                    "--dry-run",
                    "--",
                    "full",
                ],
                run=self.fake,
                environment=environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("is not an existing absolute directory", err.getvalue())
        self.assertEqual([], self.fake.commands)

    def test_arbitrary_absolute_tool_root_without_authority_is_refused(self) -> None:
        tool_root = self.split_tool_root()
        environment = dict(
            self.environment,
            DEV_HERMIT_TOOL_ROOT=str(tool_root),
            DEV_HERMIT_PARENT=str(self.root),
        )
        out = io.StringIO()
        err = io.StringIO()

        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                [
                    "--checkout",
                    str(self.checkout),
                    "--agent",
                    "owner-checkout-watch",
                    "--target",
                    SHA,
                    "--dry-run",
                    "--",
                    "full",
                ],
                run=self.fake,
                environment=environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("is not a readable Git worktree", err.getvalue())
        self.assertEqual([], self.fake.commands)

    def test_canonical_linked_worktree_fallback_remains_supported(self) -> None:
        state_root = self.root / "canonical-parent"
        linked = self.root / "canonical-linked"
        parent_main_tests.git(self.root, "init", "--initial-branch=main", str(state_root))
        parent_main_tests.git(state_root, "config", "user.name", "Authority Test")
        parent_main_tests.git(
            state_root, "config", "user.email", "authority@example.invalid"
        )
        (state_root / "tracked").write_text("fixture\n")
        parent_main_tests.git(state_root, "add", "tracked")
        parent_main_tests.git(state_root, "commit", "-m", "fixture")
        parent_main_tests.git(
            state_root,
            "worktree",
            "add",
            "-b",
            "tool-authority-test",
            str(linked),
        )
        expected = parent_main_tests.git(state_root, "rev-parse", "HEAD").stdout.strip()

        observed = start_unit.canonical_worktree_tool_head(linked, state_root)

        self.assertEqual(expected, observed)

    def test_sweep_retains_a_terminal_checkout_that_holds_uncommitted_work(self) -> None:
        fresh = self.root / "worktrees" / "validate" / "validate-fresh-sweep"
        receipt = fresh / "reports" / "result.jsonl"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(f'{{"commit": "{SHA}"}}\n')
        cargo_home = self.root / "ignored/validate/cargo-homes/validate-cargo-sweep"
        cargo_home.mkdir(parents=True)
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
                "wrkslots_slot": fresh.name,
                "wrkslots_generation": 7,
                "cargo_home": str(cargo_home),
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )

        # THE ONLY DIFFERENCE FROM THE REMOVAL CASE ABOVE. Everything else --
        # terminal unit, completed record, archived receipt, managed identity --
        # is identical, so this isolates the authorisation gate and nothing else.
        # Without it the sweep removes this tree and the uncommitted paths, which
        # exist nowhere else, are gone.
        self.fake.fresh_authored_work = (2, 0)

        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        # 75 is the sweep's own signal that it retained something it could not
        # clean, which is exactly what the gate produces. Pinning it is stronger
        # than expecting success: a caller reading 0 here would believe the
        # backlog had drained.
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, err.getvalue())
        report = json.loads(out.getvalue())
        self.assertEqual([], report["removed"])
        self.assertEqual(
            [{"checkout": str(fresh), "reason": "holds 2 uncommitted path(s)"}],
            report["retained"],
        )
        # THE ASSERTION THE WHOLE GATE EXISTS FOR: the tree is still there, and
        # so are the two paths that exist nowhere else.
        self.assertTrue(fresh.exists())

    def test_sweep_preserves_cleanup_failure_for_unreadable_authored_work(self) -> None:
        fresh, _record = self.managed_cleanup_record("unreadable-authored-work")

        with mock.patch.object(
            start_unit.tree_disposition, "authored_work", return_value=None
        ):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.exists())
        self.assertEqual(
            [
                {
                    "checkout": str(fresh),
                    "cleanup_failed": True,
                    "reason": "authored-work state could not be read",
                }
            ],
            report["retained"],
        )

    def test_sweep_retains_a_terminal_checkout_whose_commits_reach_no_remote(self) -> None:
        fresh = self.root / "worktrees" / "validate" / "validate-fresh-sweep"
        receipt = fresh / "reports" / "result.jsonl"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(f'{{"commit": "{SHA}"}}\n')
        cargo_home = self.root / "ignored/validate/cargo-homes/validate-cargo-sweep"
        cargo_home.mkdir(parents=True)
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
                "wrkslots_slot": fresh.name,
                "wrkslots_generation": 7,
                "cargo_home": str(cargo_home),
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )

        # A commit off every remote ref is the ORDINARY state of a validate
        # checkout -- it was created at a pull-request head that the landing
        # squash-rewrote. It is addressable, so the sweep asks for it to be
        # preserved rather than refusing forever.
        self.fake.fresh_authored_work = (0, 1)

        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        # 75 is the sweep's own signal that it retained something it could not
        # clean, which is exactly what the gate produces. Pinning it is stronger
        # than expecting success: a caller reading 0 here would believe the
        # backlog had drained.
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, err.getvalue())
        report = json.loads(out.getvalue())
        self.assertEqual([], report["removed"])
        self.assertEqual(1, len(report["retained"]))
        self.assertIn("rescue ref", report["retained"][0]["reason"])
        self.assertTrue(fresh.exists())

    def retire_record(self, suffix: str) -> tuple[Path, Path]:
        """A materialized Hermit validate record of the shape ExecStopPost meets."""

        fresh, record_path = self.managed_cleanup_record(suffix)
        record = json.loads(record_path.read_text())
        record["target"] = SHA
        record["e2e_result_root"] = str(self.root / "ignored/validate/artifacts" / suffix)
        Path(record["e2e_result_root"]).mkdir(parents=True, exist_ok=True)
        record_path.write_text(json.dumps(record, sort_keys=True) + "\n")
        return fresh, record_path

    def pre_dag_no_result_record(self, suffix: str) -> tuple[Path, Path, Path, Path]:
        """Plant the current typed zero-work result that cannot produce a row."""

        fresh, record_path = self.retire_record(suffix)
        log = self.root / "ignored/validate" / f"{suffix}.log"
        log.write_text("FINAL_VALIDATE_STATUS=COULD_NOT_RUN\nmissing pinned image\n")
        detail = "missing pinned image"
        record = json.loads(record_path.read_text())
        record.update(
            kind="validate",
            state="completed",
            result="no-result",
            result_source="validation-service-result",
            service_result_schema=start_unit.service_result.SCHEMA_VERSION,
            final_validate_status="COULD_NOT_RUN",
            detail=detail,
            exit_code=start_unit.EXIT_COULD_NOT_DETERMINE,
            executed_nodes=0,
            executed_tests=0,
            passed_tests=0,
            selection_mode="full",
            scorecard_writeback=None,
            started_at="2026-09-22T12:00:00+00:00",
            finished_at="2026-09-22T12:00:01+00:00",
            materialized_target=True,
            log=str(log),
            agent="manifest-plan-race",
            cargo_home=str(
                self.root / "ignored/validate/cargo-homes" / f"cargo-{suffix}"
            ),
            parent_checkout_head=SHA,
            producer=start_unit.run_registry.PRODUCER,
            admission="ci-hub validate-lock",
            pane_role="observer-only",
            validate_lock_child_deadline_seconds=3600,
            safe_ci_dag_runner_log_dir=str(
                self.root / "ignored/validate/artifacts" / suffix / "logs"
            ),
            hermit_run_timeout_seconds=3300,
        )
        record_path.write_text(json.dumps(record, sort_keys=True) + "\n")
        sidecar = start_unit.service_result.result_path(record_path)
        sidecar.write_text(
            json.dumps(
                {
                    "schema_version": start_unit.service_result.SCHEMA_VERSION,
                    "commit": SHA,
                    "profile": "full",
                    "selection_mode": "full",
                    "final_validate_status": "COULD_NOT_RUN",
                    "detail": [detail],
                    "exit_code": start_unit.EXIT_COULD_NOT_DETERMINE,
                    "executed_nodes": 0,
                    "executed_tests": 0,
                    "passed_tests": 0,
                    "scorecard_writeback": None,
                },
                sort_keys=True,
            )
            + "\n"
        )
        start_unit.run_registry.parse_current_record(record)
        return fresh, record_path, sidecar, log

    def test_retire_unit_marks_zero_work_no_result_not_applicable(self) -> None:
        fresh, record_path, sidecar, log = self.pre_dag_no_result_record(
            "zero-work-no-result"
        )
        result_root = Path(
            start_unit.run_registry.read_record(record_path)["e2e_result_root"]
        )
        sidecar_bytes = sidecar.read_bytes()
        log_bytes = log.read_bytes()

        rc, error = self.retire("zero-work-no-result")

        self.assertEqual(0, rc, error)
        self.assertIn("CELL LEDGER NOT APPLICABLE", error)
        self.assertFalse(fresh.exists())
        self.assertEqual([str(fresh)], self.fake.removed)
        self.assertTrue(self.checkout.is_dir())
        self.assertTrue(result_root.is_dir())
        self.assertTrue(record_path.is_file())
        self.assertEqual(sidecar_bytes, sidecar.read_bytes())
        self.assertEqual(log_bytes, log.read_bytes())
        durable = start_unit.run_registry.read_record(record_path)
        self.assertEqual(
            start_unit.LEDGER_MIRROR_NOT_APPLICABLE,
            durable["ledger_mirror"]["state"],
        )
        self.assertFalse(start_unit.cell_ledger_published(durable))
        self.assertTrue(
            start_unit.cell_ledger_retirement_ready(self.root, record_path, durable)
        )
        self.assertIsNone(durable["scorecard_writeback"])
        self.assertFalse(
            any(
                len(command) > 1 and command[1].endswith("ci-hub/series/mirror.py")
                for command in self.fake.commands
            )
        )
        self.assertFalse(
            any(
                len(command) > 1 and command[1] == "publish-commit-status"
                for command in self.fake.commands
            )
        )

    def test_zero_work_no_result_lock_contention_is_not_retirement_ready(
        self,
    ) -> None:
        fresh, record_path, _sidecar, _log = self.pre_dag_no_result_record(
            "zero-work-contended"
        )
        record = start_unit.run_registry.read_record(record_path)
        update_record = start_unit.run_registry.update_record

        def contended_update(
            path: Path, *, blocking: bool = True, **fields: object
        ) -> dict[str, object]:
            if "ledger_mirror" in fields:
                self.assertFalse(blocking)
                raise start_unit.run_registry.RecordLocked(
                    "fixture holds ledger_mirror bookkeeping lock"
                )
            return update_record(path, blocking=blocking, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=contended_update,
        ):
            returned = start_unit.finalize_ledger_mirror(
                self.root,
                self.root,
                record_path,
                record=record,
                run=self.fake,
            )
            returned_ready = start_unit.cell_ledger_retirement_ready(
                self.root, record_path, returned
            )
            durable = start_unit.run_registry.read_record(record_path)
            durable_ready = start_unit.cell_ledger_retirement_ready(
                self.root, record_path, durable
            )
            rc, error = self.retire("zero-work-contended")

        self.assertEqual(
            start_unit.LEDGER_MIRROR_NOT_APPLICABLE,
            returned["ledger_mirror"]["state"],
        )
        self.assertFalse(returned_ready)
        self.assertFalse(durable_ready)
        self.assertNotIn("ledger_mirror", durable)
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("RUN-RECORD NOT UPDATED", error)
        self.assertIn("RETIRE-REFUSED", error)
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any("remove-validate-batch" in command for command in self.fake.commands)
        )

    def test_zero_work_no_result_already_absent_is_idempotent(self) -> None:
        """A dead owner's unheld checkout is reclaimable before this repair runs."""

        fresh, record_path, sidecar, log = self.pre_dag_no_result_record(
            "zero-work-already-absent"
        )
        record = start_unit.finalize_ledger_mirror(
            self.root,
            self.root,
            record_path,
            record=start_unit.run_registry.read_record(record_path),
            run=self.fake,
        )
        self.assertTrue(
            start_unit.cell_ledger_retirement_ready(self.root, record_path, record)
        )
        sidecar_bytes = sidecar.read_bytes()
        log_bytes = log.read_bytes()
        shutil.rmtree(fresh)
        self.fake.commands.clear()

        rc, error = self.retire("zero-work-already-absent")

        self.assertEqual(0, rc, error)
        self.assertIn("RETIRE-ALREADY-ABSENT", error)
        self.assertFalse(
            any("remove-validate-batch" in command for command in self.fake.commands)
        )
        durable = start_unit.run_registry.read_record(record_path)
        self.assertNotIn("checkout_removed_at", durable)
        self.assertEqual(sidecar_bytes, sidecar.read_bytes())
        self.assertEqual(log_bytes, log.read_bytes())

    def test_zero_work_no_result_refuses_near_misses_and_forged_marker(self) -> None:
        cases = [
            *(
                f"{field}-nonzero"
                for field in ("executed_nodes", "executed_tests", "passed_tests")
            ),
            *(
                f"{field}-missing"
                for field in ("executed_nodes", "executed_tests", "passed_tests")
            ),
            "wrong-result",
            "wrong-status",
            "wrong-schema",
            "wrong-writeback",
            "wrong-sidecar-commit",
            "sidecar-handle-mismatch",
            "wrong-run-id",
            "forged-marker",
        ]
        for index, case in enumerate(cases):
            with self.subTest(case=case):
                _fresh, record_path, sidecar, _log = self.pre_dag_no_result_record(
                    f"near-miss-{index}"
                )
                record = start_unit.run_registry.read_record(record_path)
                result = json.loads(sidecar.read_text())
                if case.endswith("-nonzero"):
                    record[case.removesuffix("-nonzero")] = 1
                elif case.endswith("-missing"):
                    record.pop(case.removesuffix("-missing"))
                elif case == "wrong-result":
                    record["result"] = "failure"
                elif case == "wrong-status":
                    record.update(
                        result="failure",
                        final_validate_status="FAILED",
                        exit_code=1,
                        detail=None,
                    )
                elif case == "wrong-schema":
                    record["service_result_schema"] = (
                        start_unit.service_result.TEST_COUNTS_SCHEMA_VERSION
                    )
                elif case == "wrong-writeback":
                    failed = {"status": "failed", "error": "fixture writeback"}
                    record["scorecard_writeback"] = failed
                    result["scorecard_writeback"] = failed
                elif case == "wrong-sidecar-commit":
                    result["commit"] = "f" * 40
                elif case == "sidecar-handle-mismatch":
                    result["detail"] = ["different refusal"]
                elif case == "wrong-run-id":
                    record["unit"] = "validate-another-run.service"
                elif case == "forged-marker":
                    record["ledger_mirror"] = {
                        "state": start_unit.LEDGER_MIRROR_NOT_APPLICABLE,
                        "recorded_at": "2026-09-22T12:00:02+00:00",
                        "repository": start_unit.CELL_LEDGER_REPOSITORY,
                        "detail": start_unit.PRE_DAG_NO_RESULT_DETAIL,
                    }
                    sidecar.unlink()
                if case != "forged-marker":
                    sidecar.write_text(json.dumps(result, sort_keys=True) + "\n")

                self.assertFalse(
                    start_unit.pre_dag_no_result_is_authoritative(
                        self.root, record_path, record
                    )
                )
                self.assertFalse(
                    start_unit.cell_ledger_retirement_ready(
                        self.root, record_path, record
                    )
                )
                self.fake.commands.clear()
                self.fake.ledger_mirror_rc = 1
                self.fake.ledger_mirror_stderr = "publication refused"

                def bookkeep(
                    _path: Path,
                    *,
                    base_record: dict[str, object] | None = None,
                    **fields: object,
                ) -> dict[str, object]:
                    return {**(base_record or {}), **fields}

                with mock.patch.object(
                    start_unit, "bookkeep_record", side_effect=bookkeep
                ):
                    updated = start_unit.finalize_ledger_mirror(
                        self.root,
                        self.root,
                        record_path,
                        record=record,
                        run=self.fake,
                    )
                self.assertEqual("failed", updated["ledger_mirror"]["state"])
                self.assertTrue(
                    any(
                        len(command) > 1
                        and command[1].endswith("ci-hub/series/mirror.py")
                        for command in self.fake.commands
                    )
                )
                self.assertFalse(
                    any(
                        "remove-validate-batch" in command
                        for command in self.fake.commands
                    )
                )
                self.fake.ledger_mirror_rc = 0
                self.fake.ledger_mirror_stderr = ""

    def test_retire_unit_never_waits_for_the_unit_to_become_inactive(self) -> None:
        """THE PROPERTY THE ENTRY POINT EXISTS FOR, asserted directly.

        This is called from the unit's OWN ExecStopPost, where the unit is
        `deactivating` and can never reach a state in TERMINAL_STATES until the
        caller returns. Anything that polls `systemctl show` here spins until the
        stop timeout and leaves a half-deleted checkout. So the assertion is not
        "it finished quickly" -- it is that it never asked systemd at all.
        """

        self.retire_record("noyield")
        start_unit.main(
            ["--retire-unit", "validate-noyield", "--json"],
            run=self.fake,
            environment=self.environment,
            root=self.root,
        )
        systemctl = [c for c in self.fake.commands if c and c[0] == "systemctl"]
        self.assertEqual([], systemctl)

    def retire(self, suffix: str) -> tuple[int, str]:
        err = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            rc = start_unit.main(
                ["--retire-unit", f"validate-{suffix}", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )
        return rc, err.getvalue()

    def test_exec_stop_post_retains_dangling_run_handle_symlink(self) -> None:
        record = start_unit.run_registry.record_path(
            self.root, "validate-dangling-stop-post"
        )
        record.parent.mkdir(parents=True, exist_ok=True)
        record.symlink_to(record.parent / "missing-run-handle.json")

        rc, error = self.retire("dangling-stop-post")

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("run handle", error)
        self.assertIn("is a symlink", error)
        self.assertTrue(record.is_symlink())
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in self.fake.commands
                for action in ("remove", "remove-validate-batch", "recover")
            )
        )

    def test_retire_unit_attempts_no_removal_unless_ledger_published(self) -> None:
        fresh, record_path = self.retire_record("noremove")
        self.fake.ledger_mirror_rc = 1
        self.fake.ledger_mirror_stderr = "remote push refused"
        rc, err = self.retire("noremove")
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("RETIRE-REFUSED", err)
        self.assertIn("remote push refused", err)
        self.assertFalse(any("remove-validate-batch" in c for c in self.fake.commands))
        self.assertTrue(fresh.exists())
        self.assertEqual("failed", run_registry.read_record(record_path)["ledger_mirror"]["state"])

    def test_retire_unit_skips_a_record_it_does_not_own(self) -> None:
        """Returning 0 without acting beats half-handling an unfamiliar shape."""

        _fresh, record_path = self.retire_record("foreign")
        record = json.loads(record_path.read_text())
        record["repo"] = "rrnewton/reverie"
        record_path.write_text(json.dumps(record, sort_keys=True) + "\n")
        rc = start_unit.main(
            ["--retire-unit", "validate-foreign", "--json"],
            run=self.fake,
            environment=self.environment,
            root=self.root,
        )
        self.assertEqual(0, rc)

    def test_retire_unit_cannot_be_combined_with_the_sweep(self) -> None:
        rc = start_unit.main(
            ["--retire-unit", "validate-x", "--sweep-completed"],
            run=self.fake,
            environment=self.environment,
            root=self.root,
        )
        self.assertEqual(start_unit.EXIT_REFUSED, rc)


    def test_sweep_completed_archives_receipts_and_removes_checkout(self) -> None:
        fresh = self.root / "worktrees" / "validate" / "validate-fresh-sweep"
        receipt = fresh / "reports" / "result.jsonl"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(f'{{"commit": "{SHA}"}}\n')
        cargo_home = self.root / "ignored/validate/cargo-homes/validate-cargo-sweep"
        cargo_home.mkdir(parents=True)
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
                "wrkslots_slot": fresh.name,
                "wrkslots_generation": 7,
                "cargo_home": str(cargo_home),
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )

        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(0, rc, err.getvalue())
        report = json.loads(out.getvalue())
        self.assertEqual([str(fresh)], report["removed"])
        self.assertEqual([], report["removed_cargo_homes"])
        self.assertEqual([], report["retained"])
        self.assertTrue(
            any(row["checkout"] == str(cargo_home) for row in report["deferred"])
        )
        self.assertFalse(fresh.exists())
        self.assertTrue(cargo_home.exists())
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-sweep/reports/result.jsonl"
        )
        self.assertEqual(f'{{"commit": "{SHA}"}}\n', archived.read_text())
        durable = start_unit.run_registry.read_record(record_path)
        self.assertIsInstance(durable.get("checkout_removed_at"), str)
        self.assertNotIn("cargo_home_removed_at", durable)
        self.assertEqual([str(archived)], durable["archived_orphaned_receipts"])

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            second_rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )
        self.assertEqual(0, second_rc)
        self.assertEqual([], json.loads(out.getvalue())["removed"])
        self.assertEqual(
            [str(cargo_home)], json.loads(out.getvalue())["removed_cargo_homes"]
        )
        self.assertFalse(cargo_home.exists())
        self.assertIsInstance(
            start_unit.run_registry.read_record(record_path).get(
                "cargo_home_removed_at"
            ),
            str,
        )
        self.assertEqual(
            [str(archived)],
            start_unit.run_registry.read_record(record_path)[
                "archived_orphaned_receipts"
            ],
        )

    def test_a_framework_result_sidecar_is_not_a_run_handle(self) -> None:
        """⚠️ THE DEFECT, PINNED. Framework result sidecars live in the runs
        directory beside the handles and end `.json` too, so the discovery glob
        picked them up and reported each as a run handle with an unsupported
        schema. Measured on the live tree 2026-09-03: 98 of the 111 failures
        this gate reported were exactly this, and the gate was red on files that
        were precisely what they should be.
        """
        _fresh, record_path = self.managed_cleanup_record("sidecar")
        sidecar = start_unit.service_result.result_path(record_path)
        sidecar.write_text(json.dumps({"anything": "the schema is not a handle's"}))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        reasons = " ".join(
            str(row.get("reason", "")) for row in report["retained"]
        )
        self.assertNotIn(str(sidecar), reasons, report["retained"])
        self.assertEqual(1, report["discovery"]["framework_result_sidecars_skipped"])
        self.assertEqual(0, report["discovery"]["reverie_count_sidecars_skipped"])
        self.assertEqual(1, report["discovery"]["run_handle_candidates"])
        self.assertEqual(2, report["discovery"]["json_files_seen"])

    def test_a_reverie_count_sidecar_is_not_a_run_handle(self) -> None:
        from test_reverie_validate import count_transport_fixture

        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        record_path, record, binding = count_transport_fixture(runs)
        start_unit.run_registry.write_record(record_path, record)
        sidecar = start_unit.reverie_node.count_sidecar_path(record_path)
        start_unit.reverie_node.publish_count_sidecar(sidecar, binding)

        _groups, _singles, _rows, retained, discovery = (
            start_unit.discover_cleanup_groups(self.root, runs)
        )

        reasons = " ".join(
            str(row.get("reason", "")) for row in retained
        )
        self.assertNotIn(str(sidecar), reasons, retained)
        self.assertEqual(0, discovery["framework_result_sidecars_skipped"])
        self.assertEqual(1, discovery["reverie_count_sidecars_skipped"])
        self.assertEqual(1, discovery["run_handle_candidates"])
        self.assertEqual(2, discovery["json_files_seen"])

    def test_a_current_handle_wins_a_reverie_count_name_collision(self) -> None:
        from test_reverie_validate import count_transport_fixture

        runs = self.root / "ignored/validate/runs"
        runs.mkdir(parents=True, exist_ok=True)
        owner_path, owner, _binding = count_transport_fixture(runs)
        start_unit.run_registry.write_record(owner_path, owner)
        _fresh, collision = self.current_unknown_cleanup_record(
            "count.json.reverie-count",
            {
                "state": "admitted",
                "recorded_at": "2026-09-04T12:00:01+00:00",
                "run_number": 7,
            },
        )
        collision_path = runs / "validate-count.json.reverie-count.json"
        start_unit.run_registry.write_current_record(collision_path, collision)

        _groups, _singles, _rows, retained, discovery = (
            start_unit.discover_cleanup_groups(self.root, runs)
        )

        self.assertEqual([], retained)
        self.assertEqual(0, discovery["reverie_count_sidecars_skipped"])
        self.assertEqual(2, discovery["run_handle_candidates"])
        self.assertEqual(2, discovery["json_files_seen"])

    def test_a_legacy_handle_wins_a_reverie_count_name_collision(self) -> None:
        from test_reverie_validate import count_transport_fixture
        from test_signal_crosscheck import current_validation_handle

        unit = "validate-count.json.reverie-count"
        for owner_present, expected_candidates in ((False, 1), (True, 2)):
            with self.subTest(owner_present=owner_present):
                case_root = self.root / f"legacy-collision-{owner_present}"
                runs = case_root / "ignored/validate/runs"
                runs.mkdir(parents=True)
                if owner_present:
                    owner_path, owner, _binding = count_transport_fixture(runs)
                    start_unit.run_registry.write_record(owner_path, owner)
                legacy = current_validation_handle(unit)
                legacy["producer"] = "ci-hub/validate/legacy_launcher.py"
                collision_path = runs / f"{unit}.json"
                collision_path.write_text(json.dumps(legacy))

                _groups, _singles, _rows, retained, discovery = (
                    start_unit.discover_cleanup_groups(case_root, runs)
                )

                self.assertEqual([], retained)
                self.assertEqual(
                    0, discovery["reverie_count_sidecars_skipped"]
                )
                self.assertEqual(
                    expected_candidates, discovery["run_handle_candidates"]
                )

    def test_reverie_count_sidecar_requires_an_exact_current_owner(self) -> None:
        from test_reverie_validate import count_transport_fixture

        for variant in ("missing-request", "wrong-kind", "path-mismatch"):
            with self.subTest(variant=variant):
                case_root = self.root / f"count-owner-{variant}"
                runs = case_root / "ignored/validate/runs"
                runs.mkdir(parents=True)
                record_path, record, binding = count_transport_fixture(runs)
                if variant == "missing-request":
                    record.pop("reverie_count_request")
                elif variant == "wrong-kind":
                    record.pop("reverie_count_request")
                    record.update(
                        kind="validate",
                        repo="rrnewton/hermit",
                        e2e_result_root=str(case_root / "ignored/validate/e2e"),
                        safe_ci_dag_runner_log_dir=str(
                            case_root / "ignored/validate/dag"
                        ),
                        hermit_run_timeout_seconds=3300,
                    )
                else:
                    record_path = runs / "validate-elsewhere.json"
                    binding["context"] = start_unit.reverie_node.count_context(
                        record, record_path
                    )
                start_unit.run_registry.write_record(record_path, record)
                sidecar = start_unit.reverie_node.count_sidecar_path(record_path)
                start_unit.reverie_node.publish_count_sidecar(sidecar, binding)

                _groups, _singles, _rows, retained, discovery = (
                    start_unit.discover_cleanup_groups(case_root, runs)
                )

                self.assertEqual(
                    0, discovery["reverie_count_sidecars_skipped"]
                )
                self.assertTrue(
                    any(row.get("checkout") == str(sidecar) for row in retained),
                    retained,
                )

    def test_an_orphan_reverie_count_sidecar_is_reported(self) -> None:
        orphan = (
            self.root
            / "ignored/validate/runs/validate-gone.json.reverie-count.json"
        )
        orphan.parent.mkdir(parents=True, exist_ok=True)
        orphan.write_text(json.dumps({"schema": "reverie-count-handoff/v2"}))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        targets = " ".join(
            str(row.get("checkout", "")) for row in report["retained"]
        )
        reasons = " ".join(
            str(row.get("reason", "")) for row in report["retained"]
        )
        self.assertIn(str(orphan), targets, report["retained"])
        self.assertIn("Reverie count sidecar has no run handle", reasons)
        self.assertEqual(0, report["discovery"]["framework_result_sidecars_skipped"])
        self.assertEqual(0, report["discovery"]["reverie_count_sidecars_skipped"])
        self.assertEqual(1, report["discovery"]["json_files_seen"])

    def test_a_reverie_count_suffix_without_json_handle_prefix_stays_loud(self) -> None:
        near_miss = (
            self.root / "ignored/validate/runs/validate-near.reverie-count.json"
        )
        near_miss.parent.mkdir(parents=True, exist_ok=True)
        near_miss.write_text(json.dumps({"schema": "reverie-count-handoff/v2"}))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        reasons = " ".join(
            str(row.get("reason", "")) for row in report["retained"]
        )
        self.assertIn(str(near_miss), reasons, report["retained"])
        self.assertIn("unsupported schema", reasons)
        self.assertEqual(0, report["discovery"]["framework_result_sidecars_skipped"])
        self.assertEqual(0, report["discovery"]["reverie_count_sidecars_skipped"])
        self.assertEqual(1, report["discovery"]["run_handle_candidates"])
        self.assertEqual(1, report["discovery"]["json_files_seen"])

    def test_a_malformed_run_handle_is_still_loud(self) -> None:
        """The other direction, and the one that makes the exclusion narrow
        rather than a way to stop looking. A handle that is genuinely unreadable
        must still be reported."""
        broken = self.root / "ignored/validate/runs/validate-broken.json"
        broken.parent.mkdir(parents=True, exist_ok=True)
        broken.write_text("{ this is not json")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        reasons = " ".join(
            str(row.get("reason", "")) for row in report["retained"]
        )
        self.assertIn(str(broken), reasons, report["retained"])
        self.assertEqual(0, report["discovery"]["framework_result_sidecars_skipped"])
        self.assertEqual(0, report["discovery"]["reverie_count_sidecars_skipped"])
        self.assertEqual(1, report["discovery"]["json_files_seen"])
        # Seen but not a usable candidate, and that distinction is the report:
        # it was looked at and it could not be read.
        self.assertEqual(0, report["discovery"]["run_handle_candidates"])

    def test_a_sidecar_with_no_run_handle_is_reported_rather_than_dropped(self) -> None:
        """The exclusion is for sidecars that BELONG to a handle. One with no
        handle is orphaned, which is a fact about the runs directory worth
        saying, so it must not vanish into the skip."""
        orphan = (
            self.root / "ignored/validate/runs/validate-gone.service-result.json"
        )
        orphan.parent.mkdir(parents=True, exist_ok=True)
        orphan.write_text(json.dumps({"result": "orphaned"}))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        targets = " ".join(
            str(row.get("checkout", "")) for row in report["retained"]
        )
        reasons = " ".join(
            str(row.get("reason", "")) for row in report["retained"]
        )
        self.assertIn(str(orphan), targets, report["retained"])
        self.assertIn("framework result sidecar has no run handle", reasons)
        self.assertEqual(0, report["discovery"]["framework_result_sidecars_skipped"])
        self.assertEqual(0, report["discovery"]["reverie_count_sidecars_skipped"])
        self.assertEqual(1, report["discovery"]["json_files_seen"])

    def test_discovery_reports_what_it_looked_at(self) -> None:
        """"removed 0" is unreadable on its own: a sweep that considered no
        candidates and one that considered a hundred and could clean none print
        the same line."""
        for index in range(3):
            _fresh, record_path = self.managed_cleanup_record(f"counted-{index}")
            start_unit.service_result.result_path(record_path).write_text("{}")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual(6, report["discovery"]["json_files_seen"])
        self.assertEqual(3, report["discovery"]["framework_result_sidecars_skipped"])
        self.assertEqual(0, report["discovery"]["reverie_count_sidecars_skipped"])
        self.assertEqual(3, report["discovery"]["run_handle_candidates"])

    def test_sweep_bounds_backlog_and_advances_on_the_next_batch(self) -> None:
        for index in range(130):
            self.managed_cleanup_record(f"backlog-{index:03d}")

        first = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual(8, len(first["removed"]))
        self.assertEqual(122, len(first["deferred"]))
        self.assertEqual([], first["retained"])
        batches = [
            command
            for command in self.fake.commands
            if wrkslots_action(command, "remove-validate-batch")
        ]
        self.assertEqual(1, len(batches))
        self.assertEqual(8, batches[0].count("--slot"))
        self.assertTrue(all(value.endswith("=7") for value in batches[0] if "=" in value))
        self.assertEqual(
            8,
            sum(
                command[:3] == ["systemctl", "--user", "show"]
                for command in self.fake.commands
            ),
            "deferred rows must not perform per-record service or receipt work",
        )

        second = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        self.assertEqual(8, len(second["removed"]))
        self.assertEqual(114, len(second["deferred"]))

    def test_zero_census_all_retained_batch_is_valid_evidence(self) -> None:
        fresh, _record = self.managed_cleanup_record("zero-census")
        self.fake.validate_batch_zero_census = True
        self.fake.validate_batch_retained[fresh.name] = "slot no longer eligible"

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual("slot no longer eligible", report["retained"][0]["reason"])
        self.assertNotIn("invalid batch report", report["retained"][0]["reason"])
        self.assertTrue(fresh.exists())

    def test_malformed_batch_report_retains_every_requested_checkout(self) -> None:
        first, _first_record = self.managed_cleanup_record("malformed-a")
        second, _second_record = self.managed_cleanup_record("malformed-b")
        self.fake.validate_batch_output = json.dumps(
            {
                "schema": 1,
                "batch_limit": 8,
                "process_censuses": 1,
                "shared_process_censuses": 1,
                "same_uid_process_censuses": 1,
                "requested": 2,
                "removed": [{"slot": first.name, "generation": 7}],
                "retained": [],
            }
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual(2, len(report["retained"]))
        self.assertTrue(
            all("invalid batch report" in row["reason"] for row in report["retained"])
        )
        self.assertTrue(first.exists())
        self.assertTrue(second.exists())

    def test_batch_metadata_requires_exact_integer_types(self) -> None:
        expected = {"schema": 1, "batch_limit": 8, "requested": 2}
        malformed = {
            "schema": (True, 1.0, "1"),
            "batch_limit": (True, 8.0, "8"),
            "requested": (True, 2.0, "2"),
        }
        for field, values in malformed.items():
            for kind, value in zip(("bool", "float", "string"), values):
                with self.subTest(field=field, kind=kind):
                    suffix = f"metadata-{field}-{kind}"
                    first, first_record = self.managed_cleanup_record(f"{suffix}-a")
                    second, second_record = self.managed_cleanup_record(f"{suffix}-b")
                    metadata = dict(expected)
                    metadata[field] = value
                    self.fake.validate_batch_output = json.dumps(
                        {
                            **metadata,
                            "process_censuses": 1,
                            "shared_process_censuses": 1,
                            "same_uid_process_censuses": 2,
                            "removed": [
                                {"slot": first.name, "generation": 7},
                                {"slot": second.name, "generation": 7},
                            ],
                            "retained": [],
                        }
                    )

                    report = start_unit.sweep_completed_checkouts(
                        self.root, run=self.fake, tool_root=self.root
                    )

                    self.assertEqual([], report["removed"])
                    self.assertEqual([], report["bookkeeping_errors"])
                    self.assertEqual(2, len(report["retained"]))
                    self.assertTrue(
                        all(
                            "invalid batch report" in row["reason"]
                            for row in report["retained"]
                        )
                    )
                    self.assertTrue(first.exists())
                    self.assertTrue(second.exists())
                    for record_path in (first_record, second_record):
                        self.assertNotIn(
                            "checkout_removed_at",
                            start_unit.run_registry.read_record(record_path),
                        )
                        record_path.unlink()
                    shutil.rmtree(first)
                    shutil.rmtree(second)

        self.fake.validate_batch_output = None

    def test_duplicate_batch_outcome_retains_every_requested_checkout(self) -> None:
        first, _record = self.managed_cleanup_record("duplicate-outcome")
        row = {"slot": first.name, "generation": 7}
        self.fake.validate_batch_output = json.dumps(
            {
                "schema": 1,
                "batch_limit": 8,
                "process_censuses": 1,
                "shared_process_censuses": 1,
                "same_uid_process_censuses": 1,
                "requested": 1,
                "removed": [row, row],
                "retained": [],
            }
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(
            all("invalid batch report" in item["reason"] for item in report["retained"])
        )
        self.assertTrue(first.exists())

    def test_batch_report_cannot_claim_removal_without_both_censuses(self) -> None:
        for shared, same_uid in ((0, 0), (1, 0)):
            with self.subTest(shared=shared, same_uid=same_uid):
                fresh = self.root / (
                    f"worktrees/validate/validate-fresh-census-{shared}-{same_uid}"
                )
                fresh.mkdir(parents=True)
                self.fake.validate_batch_output = json.dumps(
                    {
                        "schema": 1,
                        "batch_limit": 8,
                        "process_censuses": shared,
                        "shared_process_censuses": shared,
                        "same_uid_process_censuses": same_uid,
                        "requested": 1,
                        "removed": [{"slot": fresh.name, "generation": 7}],
                        "retained": [],
                    }
                )

                outcomes = start_unit.remove_fresh_checkouts_batch(
                    self.root,
                    [(fresh, start_unit.WrkslotsIdentity(fresh.name, 7))],
                    run=self.fake,
                    tool_root=self.root,
                )

                self.assertIn("invalid batch report", outcomes[str(fresh)] or "")
                self.assertTrue(fresh.exists())

    def test_mixed_batch_updates_only_the_removed_checkout(self) -> None:
        first, first_record = self.managed_cleanup_record("mixed-a")
        second, second_record = self.managed_cleanup_record("mixed-b")
        self.fake.validate_batch_retained[second.name] = "live process still uses checkout"

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(first)], report["removed"])
        self.assertEqual(str(second), report["retained"][0]["checkout"])
        self.assertIn(
            "checkout_removed_at", start_unit.run_registry.read_record(first_record)
        )
        self.assertNotIn(
            "checkout_removed_at", start_unit.run_registry.read_record(second_record)
        )

    def test_unsafe_duplicate_record_vetoes_the_physical_checkout(self) -> None:
        fresh, _safe = self.managed_cleanup_record("duplicate-safe")
        self.managed_cleanup_record(
            "duplicate-running", checkout=fresh, state="running"
        )

        def run(command: list[str], **kwargs: object):
            if (
                command[:3] == ["systemctl", "--user", "show"]
                and "validate-duplicate-running.service" in command
            ):
                return completed(
                    command,
                    stdout=(
                        "LoadState=loaded\nActiveState=active\nSubState=running\n"
                        "InvocationID=running-fixture\n"
                    ),
                )
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(any(row["reason"] == "unit-running" for row in report["retained"]))
        self.assertFalse(
            any(wrkslots_action(command, "remove-validate-batch") for command in self.fake.commands)
        )
        self.assertTrue(fresh.exists())

    def test_running_record_does_not_recover_existing_cargo_home(self) -> None:
        fresh, record_path = self.managed_cleanup_record(
            "running-cargo", state="running"
        )
        cargo_home = self.root / "ignored/validate/cargo-homes/validate-cargo-running"
        cargo_home.mkdir(parents=True)
        start_unit.run_registry.update_record(
            record_path, blocking=False, cargo_home=str(cargo_home)
        )

        def run(command: list[str], **kwargs: object):
            if command[:3] == ["systemctl", "--user", "show"]:
                return completed(
                    command,
                    stdout=(
                        "LoadState=loaded\nActiveState=active\nSubState=running\n"
                        "InvocationID=running-cargo-fixture\n"
                    ),
                )
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual([], report["removed_cargo_homes"])
        self.assertTrue(
            any(row["reason"] == "unit-running" for row in report["retained"])
        )
        self.assertTrue(fresh.exists())
        self.assertTrue(cargo_home.exists())
        self.assertFalse(
            any(
                "--ownerless-validate-cargo-home" in command
                for command in self.fake.commands
            )
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )

    def test_conflicting_duplicate_generation_vetoes_the_physical_checkout(self) -> None:
        fresh, _first = self.managed_cleanup_record("generation-a", generation=7)
        self.managed_cleanup_record("generation-b", checkout=fresh, generation=8)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(any("disagree" in row["reason"] for row in report["retained"]))
        self.assertFalse(
            any(wrkslots_action(command, "remove-validate-batch") for command in self.fake.commands)
        )
        self.assertTrue(fresh.exists())

    def test_duplicate_archive_failure_vetoes_the_physical_checkout(self) -> None:
        fresh, _first = self.managed_cleanup_record("archive-a")
        self.managed_cleanup_record("archive-b", checkout=fresh)

        with mock.patch.object(
            start_unit,
            "archive_orphaned_receipts",
            side_effect=[[], RuntimeError("archive failed")],
        ):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual([], report["removed"])
        self.assertTrue(any("archive failed" in row["reason"] for row in report["retained"]))
        self.assertFalse(
            any(wrkslots_action(command, "remove-validate-batch") for command in self.fake.commands)
        )
        self.assertTrue(fresh.exists())

    def test_explicit_null_wrkslots_identity_is_not_treated_as_historical(self) -> None:
        fresh, record_path = self.managed_cleanup_record("null-identity")
        record = start_unit.run_registry.read_record(record_path)
        record["wrkslots_slot"] = None
        record["wrkslots_generation"] = None
        start_unit.run_registry.write_record(record_path, record)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(
            any("wrkslots slot" in row["reason"] for row in report["retained"])
        )
        self.assertFalse(any(wrkslots_action(command, "status") for command in self.fake.commands))
        self.assertTrue(fresh.exists())

    def test_partial_wrkslots_identity_is_not_treated_as_historical(self) -> None:
        fresh, record_path = self.managed_cleanup_record("partial-identity")
        record = start_unit.run_registry.read_record(record_path)
        record.pop("wrkslots_generation")
        start_unit.run_registry.write_record(record_path, record)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(any("together" in row["reason"] for row in report["retained"]))
        self.assertFalse(any(wrkslots_action(command, "status") for command in self.fake.commands))
        self.assertTrue(fresh.exists())

    def test_malformed_duplicate_identity_vetoes_the_physical_checkout(self) -> None:
        fresh, _valid = self.managed_cleanup_record("identity-valid")
        _same, malformed_path = self.managed_cleanup_record(
            "identity-null", checkout=fresh
        )
        malformed = start_unit.run_registry.read_record(malformed_path)
        malformed["wrkslots_slot"] = None
        malformed["wrkslots_generation"] = None
        start_unit.run_registry.write_record(malformed_path, malformed)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertFalse(
            any(wrkslots_action(command, "remove-validate-batch") for command in self.fake.commands)
        )
        self.assertTrue(fresh.exists())

    def test_unsupported_schema_duplicate_vetoes_the_physical_checkout(self) -> None:
        fresh, _valid = self.managed_cleanup_record("schema-valid")
        _same, invalid_path = self.managed_cleanup_record(
            "schema-invalid", checkout=fresh
        )
        invalid = json.loads(invalid_path.read_text())
        invalid["schema_version"] = 99
        invalid_path.write_text(json.dumps(invalid))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        unsupported = [
            row
            for row in report["retained"]
            if "unsupported schema" in row["reason"]
        ]
        self.assertTrue(unsupported)
        self.assertTrue(all(row.get("cleanup_failed") is True for row in unsupported))
        self.assertFalse(
            any(wrkslots_action(command, "remove-validate-batch") for command in self.fake.commands)
        )
        self.assertTrue(fresh.exists())

    def test_sweep_retains_noninteger_or_missing_run_handle_schema(self) -> None:
        missing = object()
        checkouts: list[Path] = []
        for label, schema_version in (
            ("missing", missing),
            ("boolean", True),
            ("floating", 1.0),
        ):
            fresh, record_path = self.managed_cleanup_record(f"schema-{label}")
            record = json.loads(record_path.read_text())
            if schema_version is missing:
                record.pop("schema_version")
            else:
                record["schema_version"] = schema_version
            record_path.write_text(json.dumps(record))
            with self.assertRaisesRegex(RuntimeError, "unsupported schema"):
                start_unit.read_cleanup_record(record_path)
            checkouts.append(fresh)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertGreaterEqual(
            sum(
                "unsupported schema" in row["reason"]
                for row in report["retained"]
            ),
            3,
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )
        self.assertTrue(all(checkout.exists() for checkout in checkouts))

    def test_sweep_retains_untyped_terminal_exit_codes(self) -> None:
        self.fake.collected_unit = True
        missing = object()
        checkouts: list[Path] = []
        for schema_label, schema_version in (
            ("schema-4", start_unit.service_result.TEST_COUNTS_SCHEMA_VERSION),
            ("schema-5", start_unit.service_result.SCHEMA_VERSION),
            ("legacy", None),
        ):
            for exit_label, exit_code in (
                ("missing", missing),
                ("boolean", False),
                ("floating", 0.0),
            ):
                suffix = f"{schema_label}-{exit_label}-exit"
                fresh, record_path = self.managed_cleanup_record(suffix)
                record = json.loads(record_path.read_text())
                record["result"] = "success"
                if schema_version is not None:
                    record.update(
                        service_result_schema=schema_version,
                        selection_mode="full",
                        executed_tests=1,
                        passed_tests=1,
                        scorecard_writeback=None,
                    )
                    if schema_version == start_unit.service_result.SCHEMA_VERSION:
                        record["detail"] = None
                if exit_code is missing:
                    record.pop("exit_code")
                else:
                    record["exit_code"] = exit_code
                record_path.write_text(json.dumps(record))
                checkouts.append(fresh)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(
            all(
                "not terminal" in row["reason"]
                for row in report["retained"]
            ),
            report["retained"],
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )
        self.assertTrue(all(checkout.exists() for checkout in checkouts))

    def test_deferred_malformed_group_still_reports_failure(self) -> None:
        for index in range(8):
            self.managed_cleanup_record(f"schema-earlier-{index}")
        fresh, record_path = self.managed_cleanup_record("schema-zz")
        record = json.loads(record_path.read_text())
        record["schema_version"] = 99
        record_path.write_text(json.dumps(record))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertTrue(any("unsupported schema" in row["reason"] for row in report["retained"]))
        self.assertTrue(fresh.exists())

    def test_unattributable_malformed_record_vetoes_the_batch(self) -> None:
        fresh, _record = self.managed_cleanup_record("unattributable")
        malformed = self.root / "ignored/validate/runs/broken.json"
        malformed.write_text("{")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(any("cannot read validation handle" in row["reason"] for row in report["retained"]))
        self.assertFalse(
            any(wrkslots_action(command, "remove-validate-batch") for command in self.fake.commands)
        )
        self.assertTrue(fresh.exists())

    def test_present_temporary_flag_must_be_true(self) -> None:
        paths: list[Path] = []
        for index, value in enumerate((None, 0, "true")):
            fresh, record_path = self.managed_cleanup_record(f"temporary-{index}")
            record = json.loads(record_path.read_text())
            record["temporary_checkout"] = value
            record_path.write_text(json.dumps(record))
            paths.append(fresh)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual(
            3,
            sum("temporary_checkout must be true" in row["reason"] for row in report["retained"]),
        )
        self.assertTrue(all(path.exists() for path in paths))

    def test_managed_git_directory_still_routes_through_wrkslots(self) -> None:
        fresh, _record = self.managed_cleanup_record("git-directory")
        (fresh / ".git").unlink()
        (fresh / ".git").mkdir()

        def run(command: list[str], **kwargs: object):
            if wrkslots_action(command, "remove-validate-batch"):
                return completed(command, rc=2, stderr="registry shape refused")
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(any("registry shape refused" in row["reason"] for row in report["retained"]))
        self.assertTrue(fresh.exists())

    def test_every_record_backed_cleanup_route_has_a_positive_control(self) -> None:
        for route in ("managed", "ownerless", "frozen", "direct", "cargo"):
            with self.subTest(route=route):
                target, record, payload = self.cleanup_route_fixture(
                    route, f"record-authority-positive-{route}"
                )
                self.assertTrue(
                    self.run_cleanup_route(route, target, record, payload)
                )
                self.assertFalse(target.exists())
                durable = start_unit.run_registry.read_record(record)
                stamp = (
                    "cargo_home_removed_at"
                    if route == "cargo"
                    else "checkout_removed_at"
                )
                self.assertIsInstance(durable.get(stamp), str)
                record.unlink()

    def test_every_cleanup_route_rejects_symlink_and_hardlink_records(self) -> None:
        routes = ("managed", "ownerless", "frozen", "direct", "cargo")
        for route in routes:
            for mutation in ("symlink", "hardlink"):
                with self.subTest(route=route, mutation=mutation):
                    target, record, payload = self.cleanup_route_fixture(
                        route, f"record-authority-{mutation}-{route}"
                    )
                    external = self.root / f"outside-{mutation}-{route}.json"
                    if mutation == "symlink":
                        record.rename(external)
                        record.symlink_to(external)
                    else:
                        os.link(record, external)
                    command_count = len(self.fake.commands)

                    removed = self.run_cleanup_route(
                        route, target, record, payload
                    )

                    self.assertFalse(removed)
                    self.assertTrue(target.exists() or target.is_symlink())
                    self.assertTrue(external.exists())
                    if mutation == "symlink":
                        self.assertTrue(record.is_symlink())
                    else:
                        self.assertEqual(2, os.lstat(record).st_nlink)
                    durable = json.loads(external.read_text())
                    self.assertNotIn("checkout_removed_at", durable)
                    self.assertNotIn("cargo_home_removed_at", durable)
                    commands = self.fake.commands[command_count:]
                    self.assertFalse(
                        any(
                            "remove-validate-batch" in command
                            or "recover-ownerless-validate-batch" in command
                            or "--ownerless-validate-cargo-home" in command
                            or "remove" in command
                            and command[0].endswith("ci-hub/bin/wrkslots")
                            for command in commands
                        ),
                        commands,
                    )
                    if record.is_symlink():
                        record.unlink()
                    else:
                        record.unlink(missing_ok=True)
                    external.unlink(missing_ok=True)
                    if target.is_symlink():
                        target.unlink()
                    else:
                        shutil.rmtree(target, ignore_errors=True)

    def test_every_cleanup_route_rejects_record_replacement_before_provider(
        self,
    ) -> None:
        routes = ("managed", "ownerless", "frozen", "direct", "cargo")
        for route in routes:
            with self.subTest(route=route):
                target, record, payload = self.cleanup_route_fixture(
                    route, f"record-authority-replaced-{route}"
                )
                real_read = start_unit.read_authorized_cleanup_record
                replaced = False

                def replace_after_descriptor_read(
                    authority: start_unit.CleanupRecordAuthority,
                ) -> dict[str, object]:
                    nonlocal replaced
                    durable = real_read(authority)
                    if authority.path == record and not replaced:
                        replacement = record.with_name(f".{record.name}.replacement")
                        replacement.write_bytes(record.read_bytes())
                        os.replace(replacement, record)
                        replaced = True
                    return durable

                command_count = len(self.fake.commands)
                with mock.patch.object(
                    start_unit,
                    "read_authorized_cleanup_record",
                    side_effect=replace_after_descriptor_read,
                ):
                    removed = self.run_cleanup_route(
                        route, target, record, payload
                    )

                self.assertTrue(replaced)
                self.assertFalse(removed)
                self.assertTrue(target.exists() or target.is_symlink())
                durable = json.loads(record.read_text())
                self.assertNotIn("checkout_removed_at", durable)
                self.assertNotIn("cargo_home_removed_at", durable)
                commands = self.fake.commands[command_count:]
                self.assertFalse(
                    any(
                        "remove-validate-batch" in command
                        or "recover-ownerless-validate-batch" in command
                        or "--ownerless-validate-cargo-home" in command
                        or "remove" in command
                        and command[0].endswith("ci-hub/bin/wrkslots")
                        for command in commands
                    ),
                    commands,
                )
                record.unlink()
                if target.is_symlink():
                    target.unlink()
                else:
                    shutil.rmtree(target, ignore_errors=True)

    def test_every_cleanup_route_detects_record_replacement_during_provider(
        self,
    ) -> None:
        routes = ("managed", "ownerless", "frozen", "direct", "cargo")
        for route in routes:
            with self.subTest(route=route):
                target, record, payload = self.cleanup_route_fixture(
                    route, f"record-authority-provider-replaced-{route}"
                )
                provider_reached = False
                provider_succeeded = False

                def replace_inside_provider(
                    command: list[str], **_kwargs: object
                ) -> subprocess.CompletedProcess[str]:
                    nonlocal provider_reached, provider_succeeded
                    is_provider = (
                        "remove-validate-batch" in command
                        or "recover-ownerless-validate-batch" in command
                        or "--ownerless-validate-cargo-home" in command
                        or (
                            "remove" in command
                            and command[0].endswith("ci-hub/bin/wrkslots")
                        )
                    )
                    if not is_provider:
                        return self.fake(command, **_kwargs)
                    provider_reached = True
                    replacement = record.with_name(f".{record.name}.provider")
                    replacement.write_bytes(record.read_bytes())
                    os.replace(replacement, record)
                    result = self.fake(command, **_kwargs)
                    provider_succeeded = result.returncode == 0
                    return result

                removed = self.run_cleanup_route(
                    route,
                    target,
                    record,
                    payload,
                    run=replace_inside_provider,
                )

                self.assertTrue(provider_reached)
                self.assertTrue(provider_succeeded)
                self.assertFalse(removed)
                # A record mutation from outside the writer protocol can land
                # while an opaque provider call is already deleting.  The
                # descriptor check cannot undo that provider action, but it
                # must detect the authority loss and refuse to report or stamp
                # success.  Returning a provider refusal here would make this
                # test pass even if the post-provider identity check vanished.
                self.assertFalse(target.exists() or target.is_symlink())
                durable = json.loads(record.read_text())
                self.assertNotIn("checkout_removed_at", durable)
                self.assertNotIn("cargo_home_removed_at", durable)
                record.unlink()
                if target.is_symlink():
                    target.unlink()
                else:
                    shutil.rmtree(target, ignore_errors=True)

    def test_every_cleanup_route_detects_record_replacement_during_bookkeeping(
        self,
    ) -> None:
        routes = ("managed", "ownerless", "frozen", "direct", "cargo")
        real_update = start_unit.run_registry.update_record
        for route in routes:
            with self.subTest(route=route):
                target, record, payload = self.cleanup_route_fixture(
                    route, f"record-authority-bookkeeping-replaced-{route}"
                )
                replaced = False

                def replace_after_trusted_update(
                    path: Path, *, blocking: bool = True, **fields: object
                ) -> dict[str, object]:
                    nonlocal replaced
                    updated = real_update(path, blocking=blocking, **fields)
                    if Path(path) == record and (
                        "checkout_removed_at" in fields
                        or "cargo_home_removed_at" in fields
                    ):
                        replacement = record.with_name(
                            f".{record.name}.bookkeeping"
                        )
                        # Preserve the writer's exact bytes.  A pathname reopen
                        # plus byte comparison would accept this replacement;
                        # only the descriptor opened before os.replace can
                        # distinguish the inode that bookkeeping actually wrote.
                        replacement.write_bytes(record.read_bytes())
                        os.replace(replacement, record)
                        replaced = True
                    return updated

                with mock.patch.object(
                    start_unit.run_registry,
                    "update_record",
                    side_effect=replace_after_trusted_update,
                ):
                    removed = self.run_cleanup_route(
                        route, target, record, payload
                    )

                self.assertTrue(replaced)
                self.assertFalse(removed)
                self.assertFalse(target.exists() or target.is_symlink())
                durable = json.loads(record.read_text())
                stamp = (
                    "cargo_home_removed_at"
                    if route == "cargo"
                    else "checkout_removed_at"
                )
                self.assertIsInstance(durable.get(stamp), str)
                record.unlink()

    def test_rollout_fence_blocks_exact_4037_public_writer_race(self) -> None:
        old_registry = self.historical_module(
            "ci-hub/validate/run_registry.py", "legacy_4037_run_registry"
        )
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "rollout-old-public-writer", temporary=False, current=True
        )
        second = record.with_name("validate-rollout-old-public-writer-second.json")
        writer_ready = threading.Event()
        writer_continue = threading.Event()
        writer_errors: list[BaseException] = []
        real_write = old_registry._write_unlocked

        def delayed_write(path: Path, value: dict[str, object]) -> None:
            writer_ready.set()
            if not writer_continue.wait(timeout=2):
                raise RuntimeError("test did not release exact 4037 writer")
            real_write(path, value)

        old_registry._write_unlocked = delayed_write

        def write_old_owner() -> None:
            try:
                old_registry.write_record(
                    second,
                    {
                        "schema_version": 1,
                        "unit": "validate-rollout-old-public-writer-second.service",
                        "source_checkout": str(self.checkout),
                        "cargo_home": str(cargo_home),
                        "repo": "rrnewton/hermit",
                        "state": "running",
                    },
                )
            except BaseException as error:
                writer_errors.append(error)

        writer = threading.Thread(target=write_old_owner, daemon=True)
        writer.start()
        self.assertTrue(writer_ready.wait(timeout=1), "exact 4037 writer did not start")
        provider_called = False

        def provider(command: list[str], **kwargs: object):
            nonlocal provider_called
            provider_called = True
            return self.fake(command, **kwargs)

        try:
            with mock.patch.object(
                start_unit, "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED", False
            ):
                cleaned, reason = start_unit.cleanup_recorded_cargo_home(
                    self.root,
                    start_unit.run_registry.read_record(record),
                    record_path=record,
                    run=provider,
                    tool_root=self.root,
                )
        finally:
            writer_continue.set()
            writer.join(timeout=2)

        self.assertFalse(writer.is_alive(), "exact 4037 writer deadlocked")
        self.assertEqual([], writer_errors)
        self.assertTrue(second.exists())
        self.assertFalse(cleaned)
        self.assertIn("rollout-fenced", reason or "")
        self.assertFalse(provider_called)
        self.assertTrue(cargo_home.exists())
        self.assertNotIn(
            "cargo_home_removed_at", start_unit.run_registry.read_record(record)
        )

    def test_source_rollout_fence_disables_every_record_backed_cleanup_route(
        self,
    ) -> None:
        self.assertIs(PRODUCTION_CLEANUP_ROLLOUT_ENABLED, False)
        marker = self.root / "ignored/validate/cleanup-rollout-enabled"
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text("enabled\n")
        for route in ("managed", "ownerless", "frozen", "direct", "cargo"):
            with self.subTest(route=route):
                target, record, payload = self.cleanup_route_fixture(
                    route, f"rollout-source-fence-{route}"
                )
                command_count = len(self.fake.commands)
                with mock.patch.object(
                    start_unit, "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED", False
                ):
                    removed = self.run_cleanup_route(
                        route, target, record, payload
                    )

                self.assertFalse(removed)
                self.assertTrue(target.exists() or target.is_symlink())
                commands = self.fake.commands[command_count:]
                self.assertFalse(
                    any(
                        "remove-validate-batch" in command
                        or "recover-ownerless-validate-batch" in command
                        or "--ownerless-validate-cargo-home" in command
                        or (
                            "remove" in command
                            and command[0].endswith("ci-hub/bin/wrkslots")
                        )
                        for command in commands
                    ),
                    commands,
                )
                durable = start_unit.run_registry.read_record(record)
                self.assertNotIn("checkout_removed_at", durable)
                self.assertNotIn("cargo_home_removed_at", durable)
                record.unlink()
                if target.is_symlink():
                    target.unlink()
                else:
                    shutil.rmtree(target, ignore_errors=True)

    def test_rollout_fence_covers_exact_4037_pressure_launch_call_path(
        self,
    ) -> None:
        old_registry = self.historical_module(
            "ci-hub/validate/run_registry.py", "legacy_4037_pressure_registry"
        )
        old_pressure = self.historical_module(
            "ci-hub/series/pressure_schedule.py",
            "legacy_4037_pressure_schedule",
            run_registry_module=old_registry,
        )
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "rollout-old-pressure", temporary=False, current=True
        )
        writer_ready = threading.Event()
        writer_continue = threading.Event()
        writer_errors: list[BaseException] = []
        launcher_results: list[int] = []
        real_write = old_registry._write_unlocked
        delayed_once = False

        def delayed_write(path: Path, value: dict[str, object]) -> None:
            nonlocal delayed_once
            if not delayed_once:
                delayed_once = True
                writer_ready.set()
                if not writer_continue.wait(timeout=2):
                    raise RuntimeError("test did not release exact 4037 launcher")
            real_write(path, value)

        old_registry._write_unlocked = delayed_write

        def launch_run(
            command: list[str], **_kwargs: object
        ) -> subprocess.CompletedProcess[str]:
            if command[0].endswith("systemctl"):
                return completed(command, rc=3)
            if command[-2:] == ["rev-parse", "origin/main"]:
                return completed(command, stdout=f"{SHA}\n")
            return completed(command)

        def launch_old_pressure() -> None:
            try:
                launcher_results.append(old_pressure.launch(self.root, run=launch_run))
            except BaseException as error:
                writer_errors.append(error)

        with mock.patch.object(
            old_pressure.shutil,
            "which",
            side_effect=lambda name: f"/usr/bin/{name}",
        ):
            launcher = threading.Thread(target=launch_old_pressure, daemon=True)
            launcher.start()
            self.assertTrue(
                writer_ready.wait(timeout=1),
                "exact 4037 pressure launcher did not reach its record writer",
            )
            try:
                with mock.patch.object(
                    start_unit, "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED", False
                ):
                    cleaned, reason = start_unit.cleanup_recorded_cargo_home(
                        self.root,
                        start_unit.run_registry.read_record(record),
                        record_path=record,
                        run=self.fake,
                        tool_root=self.root,
                    )
            finally:
                writer_continue.set()
                launcher.join(timeout=2)

        self.assertFalse(launcher.is_alive(), "exact 4037 pressure launcher deadlocked")
        self.assertEqual([], writer_errors)
        self.assertEqual([0], launcher_results)
        self.assertTrue(
            old_registry.record_path(self.root, old_pressure.UNIT).exists()
        )
        self.assertFalse(cleaned)
        self.assertIn("rollout-fenced", reason or "")
        self.assertTrue(cargo_home.exists())

    def test_rollout_fence_covers_exact_4037_start_unit_create_path(self) -> None:
        old_registry = self.historical_module(
            "ci-hub/validate/run_registry.py", "legacy_4037_start_registry"
        )
        old_start = self.historical_module(
            "ci-hub/validate/start_unit.py",
            "legacy_4037_start_unit",
            run_registry_module=old_registry,
        )
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "rollout-old-start", temporary=False, current=True
        )
        old_runtime = self.root / "old-start-runtime"
        old_runtime.mkdir()
        old_cargo = (
            self.root
            / "ignored/validate/cargo-homes/validate-cargo-old-start-unit"
        )
        old_cargo.mkdir(parents=True)
        writer_ready = threading.Event()
        writer_continue = threading.Event()
        writer_errors: list[BaseException] = []
        launcher_results: list[int] = []
        real_create = old_registry.create_current_record

        def delayed_create(path: Path, value: dict[str, object]) -> None:
            writer_ready.set()
            if not writer_continue.wait(timeout=2):
                raise RuntimeError("test did not release exact 4037 start-unit")
            real_create(path, value)
            raise RuntimeError("stop exact 4037 launch after its production create")

        def launch_old_start_unit() -> None:
            try:
                launcher_results.append(
                    old_start.main(
                        [
                            "--checkout",
                            str(self.checkout),
                            "--agent",
                            "old-start-test",
                            "--target",
                            SHA,
                            "--unit",
                            "validate-old-start-unit",
                            "--in-place",
                            "--",
                            "quick",
                        ],
                        run=self.fake,
                        environment=self.environment,
                        root=self.root,
                        sleep=lambda _seconds: None,
                    )
                )
            except BaseException as error:
                writer_errors.append(error)

        patches = [
            mock.patch.object(
                old_start, "validate_checkout", return_value=self.checkout
            ),
            mock.patch.object(
                old_start, "source_checkout_branch", return_value="old-branch"
            ),
            mock.patch.object(old_start, "preflight", return_value=None),
            mock.patch.object(
                old_start, "prepare_runtime_root", return_value=old_runtime
            ),
            mock.patch.object(
                old_start, "prepare_private_cargo_home", return_value=old_cargo
            ),
            mock.patch.object(
                old_start, "build_systemd_command", return_value=["systemd-run"]
            ),
            mock.patch.object(old_start, "detached_tool_prefix", return_value=None),
            mock.patch.object(old_start, "parent_checkout_head", return_value=SHA),
            mock.patch.object(
                old_start.gh_route, "executable", return_value=sys.executable
            ),
            mock.patch.object(
                old_registry, "create_current_record", side_effect=delayed_create
            ),
        ]
        for patcher in patches:
            patcher.start()
        try:
            launcher = threading.Thread(target=launch_old_start_unit, daemon=True)
            launcher.start()
            self.assertTrue(
                writer_ready.wait(timeout=1),
                "exact 4037 start_unit did not reach its production record create",
            )
            with mock.patch.object(
                start_unit, "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED", False
            ):
                cleaned, reason = start_unit.cleanup_recorded_cargo_home(
                    self.root,
                    start_unit.run_registry.read_record(record),
                    record_path=record,
                    run=self.fake,
                    tool_root=self.root,
                )
        finally:
            writer_continue.set()
            if "launcher" in locals():
                launcher.join(timeout=2)
            for patcher in reversed(patches):
                patcher.stop()

        self.assertFalse(launcher.is_alive(), "exact 4037 start_unit deadlocked")
        self.assertEqual([], writer_errors)
        self.assertEqual([start_unit.EXIT_REFUSED], launcher_results)
        self.assertTrue(
            old_registry.record_path(self.root, "validate-old-start-unit").exists()
        )
        self.assertFalse(cleaned)
        self.assertIn("rollout-fenced", reason or "")
        self.assertTrue(cargo_home.exists())
        # The supported old launcher allocated its own unique Cargo directory;
        # that target-specific fact narrows exposure but cannot fence arbitrary
        # old public writers, which the preceding regression exercises.
        self.assertNotEqual(cargo_home, old_cargo)

    def test_generation_lock_excludes_cleanup_until_launcher_releases(self) -> None:
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "launcher-generation-lock", temporary=False, current=True
        )
        payload = start_unit.run_registry.read_record(record)
        with start_unit.run_registry.run_writer_generation_lock(self.root):
            cleaned, reason = start_unit.cleanup_recorded_cargo_home(
                self.root,
                payload,
                record_path=record,
                run=self.fake,
                tool_root=self.root,
            )
        self.assertFalse(cleaned)
        self.assertIn("run-writer generation authority", reason or "")
        self.assertTrue(cargo_home.exists())

        cleaned, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            payload,
            record_path=record,
            run=self.fake,
            tool_root=self.root,
        )
        self.assertTrue(cleaned, reason)
        self.assertFalse(cargo_home.exists())

    def test_start_unit_bridges_storage_creation_to_initial_record_with_generation_lock(
        self,
    ) -> None:
        runtime = self.root / "launcher-generation-runtime"
        runtime.mkdir()
        cargo = (
            self.root
            / "ignored/validate/cargo-homes/validate-cargo-launcher-generation"
        )
        cargo.mkdir(parents=True)
        real_create = start_unit.run_registry.create_current_record
        blocked_during_storage = False
        blocked_during_create = False
        exclusive_after_create = False

        def allocate_while_shared(*_args: object, **_kwargs: object) -> Path:
            nonlocal blocked_during_storage
            with self.assertRaises(start_unit.run_registry.RecordLocked):
                with start_unit.run_registry.run_writer_generation_lock(
                    self.root, exclusive=True, blocking=False
                ):
                    pass
            blocked_during_storage = True
            return cargo

        def create_while_shared(path: Path, value: dict[str, object]) -> None:
            nonlocal blocked_during_create
            with self.assertRaises(start_unit.run_registry.RecordLocked):
                with start_unit.run_registry.run_writer_generation_lock(
                    self.root, exclusive=True, blocking=False
                ):
                    pass
            blocked_during_create = True
            real_create(path, value)

        def reserve_after_release(_path: Path) -> None:
            nonlocal exclusive_after_create
            with start_unit.run_registry.run_writer_generation_lock(
                self.root, exclusive=True, blocking=False
            ):
                exclusive_after_create = True
            raise RuntimeError("stop after observing launcher-generation release")

        with (
            mock.patch.object(
                start_unit, "validate_checkout", return_value=self.checkout
            ),
            mock.patch.object(
                start_unit, "source_checkout_branch", return_value="branch"
            ),
            mock.patch.object(start_unit, "preflight", return_value=None),
            mock.patch.object(
                start_unit, "prepare_runtime_root", return_value=runtime
            ),
            mock.patch.object(
                start_unit,
                "prepare_private_cargo_home",
                side_effect=allocate_while_shared,
            ),
            mock.patch.object(
                start_unit, "build_systemd_command", return_value=["systemd-run"]
            ),
            mock.patch.object(start_unit, "detached_tool_prefix", return_value=None),
            mock.patch.object(start_unit, "parent_checkout_head", return_value=SHA),
            mock.patch.object(
                start_unit.gh_route, "executable", return_value=sys.executable
            ),
            mock.patch.object(
                start_unit.service_result, "schema_for_checkout", return_value=None
            ),
            mock.patch.object(
                start_unit.run_registry,
                "create_current_record",
                side_effect=create_while_shared,
            ),
            mock.patch.object(
                start_unit.run_registry,
                "reserve_log",
                side_effect=reserve_after_release,
            ),
        ):
            rc, _output, error = self.invoke(
                ["--in-place", "--", "quick"], unit="validate-generation-window"
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertTrue(blocked_during_storage)
        self.assertTrue(blocked_during_create)
        self.assertTrue(exclusive_after_create)

    def test_initial_record_directory_sync_failure_retains_storage_before_release(
        self,
    ) -> None:
        runtime = self.root / "launcher-durability-runtime"
        runtime.mkdir()
        cargo = (
            self.root
            / "ignored/validate/cargo-homes/validate-cargo-launcher-durability"
        )
        cargo.mkdir(parents=True)
        real_fsync = os.fsync
        file_synced = False
        directory_sync_attempted = False
        generation_held_during_sync = False

        def fail_parent_sync(descriptor: int) -> None:
            nonlocal file_synced, directory_sync_attempted
            nonlocal generation_held_during_sync
            if stat.S_ISDIR(os.fstat(descriptor).st_mode):
                directory_sync_attempted = True
                self.assertTrue(
                    file_synced,
                    "the temporary record must be fsynced before its directory",
                )
                with self.assertRaises(start_unit.run_registry.RecordLocked):
                    with start_unit.run_registry.run_writer_generation_lock(
                        self.root, exclusive=True, blocking=False
                    ):
                        pass
                generation_held_during_sync = True
                raise OSError(
                    errno.EIO, "injected initial record directory sync failure"
                )
            file_synced = True
            real_fsync(descriptor)

        with (
            mock.patch.object(
                start_unit, "validate_checkout", return_value=self.checkout
            ),
            mock.patch.object(
                start_unit, "source_checkout_branch", return_value="branch"
            ),
            mock.patch.object(start_unit, "preflight", return_value=None),
            mock.patch.object(
                start_unit, "prepare_runtime_root", return_value=runtime
            ),
            mock.patch.object(
                start_unit, "prepare_private_cargo_home", return_value=cargo
            ),
            mock.patch.object(
                start_unit, "build_systemd_command", return_value=["systemd-run"]
            ),
            mock.patch.object(start_unit, "detached_tool_prefix", return_value=None),
            mock.patch.object(start_unit, "parent_checkout_head", return_value=SHA),
            mock.patch.object(
                start_unit.gh_route, "executable", return_value=sys.executable
            ),
            mock.patch.object(
                start_unit.service_result, "schema_for_checkout", return_value=None
            ),
            mock.patch.object(
                start_unit.run_registry.os,
                "fsync",
                side_effect=fail_parent_sync,
            ),
        ):
            rc, _output, error = self.invoke(
                ["--in-place", "--", "quick"], unit="validate-durability-window"
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc, error)
        self.assertTrue(file_synced)
        self.assertTrue(directory_sync_attempted)
        self.assertTrue(generation_held_during_sync)
        self.assertIn("initial record directory sync failure", error)
        self.assertIn("not confirmed directory-durable", error)
        self.assertTrue(runtime.is_dir())
        self.assertTrue(cargo.is_dir())
        record = self.root / "ignored/validate/runs/validate-durability-window.json"
        self.assertEqual(
            "preparing", start_unit.run_registry.read_record(record)["state"]
        )
        self.assertEqual([], self.fake.removed)
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )
        with start_unit.run_registry.run_writer_generation_lock(
            self.root, exclusive=True, blocking=False
        ):
            pass

    def test_managed_population_authority_survives_runs_replacement(self) -> None:
        fresh, record = self.managed_cleanup_record(
            "managed-population-parent-replacement"
        )
        self.assert_provider_parent_replacement_is_serialized(
            fresh,
            record,
            action="remove-validate-batch",
        )

    def test_managed_provider_cross_thread_writer_fails_without_deadlock(self) -> None:
        fresh, record = self.managed_cleanup_record("managed-thread-writer")
        writer_errors: list[BaseException] = []
        writer_finished_inside_provider = False
        writer: threading.Thread | None = None

        def write_from_callback() -> None:
            try:
                start_unit.run_registry.update_record(
                    record, concurrent_writer="callback-thread"
                )
            except BaseException as error:
                writer_errors.append(error)

        def run(command: list[str], **_kwargs: object):
            nonlocal writer_finished_inside_provider, writer
            if wrkslots_action(command, "remove-validate-batch"):
                writer = threading.Thread(target=write_from_callback, daemon=True)
                writer.start()
                writer.join(timeout=1)
                writer_finished_inside_provider = not writer.is_alive()
                return completed(command, rc=3, stderr="fixture retained")
            return self.fake(command, **_kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )
        assert writer is not None
        writer.join(timeout=1)

        self.assertTrue(writer_finished_inside_provider, "callback writer deadlocked")
        self.assertFalse(writer.is_alive(), "callback writer outlived cleanup")
        self.assertEqual(1, len(writer_errors), writer_errors)
        self.assertIsInstance(writer_errors[0], start_unit.run_registry.RecordLocked)
        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        durable = start_unit.run_registry.read_record(record)
        self.assertNotIn("concurrent_writer", durable)
        self.assertNotIn("checkout_removed_at", durable)

    def test_managed_population_authority_is_held_through_bookkeeping(self) -> None:
        fresh, record = self.managed_cleanup_record("managed-bookkeeping-authority")
        real_update = start_unit.run_registry.update_record
        observed_authority = False

        def checked_update(
            path: Path, *, blocking: bool = True, **fields: object
        ) -> dict[str, object]:
            nonlocal observed_authority
            if "checkout_removed_at" in fields:
                authority = fields.get("_population_authority")
                self.assertIsInstance(
                    authority, start_unit.run_registry.RunPopulationAuthority
                )
                assert isinstance(
                    authority, start_unit.run_registry.RunPopulationAuthority
                )
                start_unit.run_registry.require_run_population_authority(
                    self.root, authority
                )
                self.assertIs(fields.get("_record_lock_held"), True)
                observed_authority = True
            return real_update(path, blocking=blocking, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=checked_update,
        ):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertTrue(observed_authority)
        self.assertEqual([str(fresh)], report["removed"], report)
        self.assertIsInstance(
            start_unit.run_registry.read_record(record).get(
                "checkout_removed_at"
            ),
            str,
        )

    def test_cleanup_stamp_parent_fsync_failure_is_reported(self) -> None:
        fresh, record = self.managed_cleanup_record("stamp-parent-fsync")
        self.fake.fresh = fresh
        self.fake.registered_validate_slots.add(
            (fresh.name, self.fake.wrkslots_generation)
        )
        payload = start_unit.run_registry.read_record(record)
        real_fsync = os.fsync
        refused = False
        directory_syncs = 0

        def fail_record_parent_sync(descriptor: int) -> None:
            nonlocal refused, directory_syncs
            metadata = os.fstat(descriptor)
            if stat.S_ISDIR(metadata.st_mode):
                directory_syncs += 1
                if directory_syncs == 2:
                    refused = True
                    raise OSError(
                        errno.EIO, "injected cleanup stamp parent fsync failure"
                    )
            real_fsync(descriptor)

        with mock.patch.object(
            start_unit.os, "fsync", side_effect=fail_record_parent_sync
        ):
            cleaned, _archived, reason = start_unit.cleanup_recorded_checkout(
                self.root,
                payload,
                record_path=record,
                unit=record.stem,
                run=self.fake,
                tool_root=self.root,
            )

        self.assertTrue(refused)
        self.assertFalse(cleaned)
        self.assertIn("cleanup stamp parent fsync failure", reason or "")
        self.assertFalse(fresh.exists() or fresh.is_symlink())
        # The replacement is visible but was not claimed durable.  Keeping the
        # failure explicit prevents a successful cleanup result from relying on
        # the unsynced directory entry.
        self.assertIn(
            "checkout_removed_at", start_unit.run_registry.read_record(record)
        )

    def test_batch_persists_intent_if_crash_precedes_checkout_stamp(
        self,
    ) -> None:
        fresh, record = self.managed_cleanup_record("batch-crash-before-stamp")
        real_update = start_unit.update_authorized_cleanup_record
        interrupted = False

        def interrupt_first_checkout_stamp(
            authority: start_unit.CleanupRecordAuthority,
            population: run_registry.RunPopulationAuthority,
            **fields: object,
        ) -> dict[str, object]:
            nonlocal interrupted
            if not interrupted and "checkout_removed_at" in fields:
                interrupted = True
                raise KeyboardInterrupt("injected crash before checkout stamp")
            return real_update(authority, population, **fields)

        with mock.patch.object(
            start_unit,
            "update_authorized_cleanup_record",
            side_effect=interrupt_first_checkout_stamp,
        ):
            with self.assertRaisesRegex(KeyboardInterrupt, "injected crash"):
                start_unit.sweep_completed_checkouts(
                    self.root, run=self.fake, tool_root=self.root
                )

        self.assertTrue(interrupted)
        self.assertFalse(fresh.exists() or fresh.is_symlink())
        interrupted_record = run_registry.read_record(record)
        self.assertIn("checkout_removal_intent", interrupted_record)
        self.assertNotIn("checkout_removed_at", interrupted_record)

    def test_managed_batch_does_not_confirm_dangling_symlink_as_removed(self) -> None:
        fresh, record = self.managed_cleanup_record("managed-dangling")

        def leave_dangling(command: list[str], **kwargs: object):
            result = self.fake(command, **kwargs)
            if wrkslots_action(command, "remove-validate-batch"):
                fresh.symlink_to(fresh.parent / "missing-managed-target")
            return result

        report = start_unit.sweep_completed_checkouts(
            self.root, run=leave_dangling, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_symlink())
        self.assertTrue(
            any("still exists" in row["reason"] for row in report["retained"]),
            report,
        )
        self.assertNotIn(
            "checkout_removed_at", start_unit.run_registry.read_record(record)
        )

    def test_sweep_discovers_preexisting_dangling_checkout_symlink(self) -> None:
        fresh, record = self.managed_cleanup_record("managed-preexisting-dangling")
        shutil.rmtree(fresh)
        fresh.symlink_to(fresh.parent / "missing-managed-target")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_symlink())
        self.assertTrue(
            any(row.get("checkout") == str(fresh) for row in report["retained"]),
            report,
        )
        self.assertNotIn(
            "checkout_removed_at", start_unit.run_registry.read_record(record)
        )

    def test_malformed_cursor_refuses_without_removing(self) -> None:
        fresh, _record = self.managed_cleanup_record("bad-cursor")
        cursor = self.root / "ignored/validate/checkout-cleanup-cursor.json"
        for kind, schema in (("bool", True), ("float", 1.0), ("string", "1")):
            with self.subTest(kind=kind):
                payload = json.dumps(
                    {"after": "validate-fresh-prior", "schema": schema}, sort_keys=True
                ) + "\n"
                cursor.write_text(payload)
                command_count = len(self.fake.commands)

                report = start_unit.sweep_completed_checkouts(
                    self.root, run=self.fake, tool_root=self.root
                )

                self.assertEqual([], report["removed"])
                self.assertEqual(
                    [str(fresh)], [row["checkout"] for row in report["deferred"]]
                )
                self.assertTrue(
                    any(
                        "invalid shape" in row["reason"]
                        for row in report["bookkeeping_errors"]
                    )
                )
                self.assertEqual(payload, cursor.read_text())
                self.assertFalse(
                    any(
                        wrkslots_action(command, "remove-validate-batch")
                        for command in self.fake.commands[command_count:]
                    )
                )
                self.assertTrue(fresh.exists())

    def test_cursor_advances_past_provider_retained_groups_and_wraps(self) -> None:
        first: list[Path] = []
        for index in range(8):
            fresh, _record = self.managed_cleanup_record(f"retained-{index:02d}")
            first.append(fresh)
            self.fake.validate_batch_retained[fresh.name] = "provider retained"
        later, _record = self.managed_cleanup_record("retained-zz")

        initial = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        self.assertEqual([], initial["removed"])
        self.assertEqual([str(later)], [row["checkout"] for row in initial["deferred"]])

        following = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        self.assertEqual([str(later)], following["removed"])
        self.assertFalse(later.exists())
        self.assertTrue(all(path.exists() for path in first))

    def test_persistently_running_early_groups_do_not_starve_a_later_checkout(self) -> None:
        running_units: set[str] = set()
        for index in range(8):
            _fresh, record_path = self.managed_cleanup_record(f"fair-{index:02d}")
            historical = json.loads(record_path.read_text())
            historical["producer"] = start_unit.run_registry.PRODUCER
            record_path.write_text(json.dumps(historical))
            with self.assertRaises(RuntimeError):
                start_unit.run_registry.read_record(record_path)
            running_units.add(
                str(historical["unit"])
            )
        ready, _ready_record = self.managed_cleanup_record("fair-zz-ready")

        def run(command: list[str], **kwargs: object):
            if command[:3] == ["systemctl", "--user", "show"] and any(
                unit in command for unit in running_units
            ):
                return completed(
                    command,
                    stdout=(
                        "LoadState=loaded\nActiveState=active\nSubState=running\n"
                        "InvocationID=running-fixture\n"
                    ),
                )
            return self.fake(command, **kwargs)

        first = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )
        self.assertEqual([], first["removed"])
        self.assertEqual([str(ready)], [row["checkout"] for row in first["deferred"]])

        second = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )
        self.assertEqual([str(ready)], second["removed"])
        self.assertFalse(ready.exists())

    def test_sweep_completed_human_report_does_not_require_launch_fields(self) -> None:
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--sweep-completed"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(0, rc, err.getvalue())
        self.assertIn("validate-run: SWEEP-COMPLETED", out.getvalue())
        self.assertIn("removed=0", out.getvalue())
        self.assertIn("removed-cargo-homes=0", out.getvalue())
        self.assertIn("removed-runtime-roots=0", out.getvalue())
        self.assertIn("retained=0", out.getvalue())
        self.assertIn("bookkeeping-errors=0", out.getvalue())

    def test_committed_wrkslots_exposes_ownerless_cargo_home_recovery(self) -> None:
        wrapper = start_unit.ROOT / "ci-hub/bin/wrkslots"

        result = subprocess.run(
            [str(wrapper), "recover", "--help"],
            cwd=start_unit.ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("--ownerless-validate-cargo-home", result.stdout)

    def test_sweep_accepts_historical_cargo_home_location(self) -> None:
        cargo_home = self.root / "worktrees/validate/validate-cargo-legacy"
        cargo_home.mkdir(parents=True)
        record_path = self.root / "ignored/validate/runs/validate-cargo-legacy.json"
        record = {
            "schema_version": 1,
            "cargo_home": str(cargo_home),
            "state": "completed",
            "exit_code": 0,
            "final_validate_status": "PASSED",
        }
        start_unit.run_registry.write_record(record_path, record)

        removed, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            record,
            record_path=record_path,
            run=self.fake,
        )

        self.assertTrue(removed)
        self.assertIsNone(reason)
        self.assertFalse(cargo_home.exists())
        recovery = next(
            command
            for command in self.fake.commands
            if "--ownerless-validate-cargo-home" in command
        )
        self.assertIn("--coordinator-authorized", recovery)
        self.assertEqual(
            "worktrees/validate/validate-cargo-legacy",
            recovery[recovery.index("--ownerless-validate-cargo-home") + 1],
        )
        self.assertEqual(
            "ignored/validate/runs/validate-cargo-legacy.json",
            recovery[recovery.index("--completed-record") + 1],
        )
        self.assertFalse(
            any(command[:2] == ["rm", "-rf"] for command in self.fake.commands)
        )

    def test_cargo_owner_cannot_appear_between_census_and_provider(self) -> None:
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "cargo-owner-interleaving", temporary=False, current=True
        )
        second = record.with_name("validate-cargo-owner-interleaving-second.json")
        writer_rejected = False

        def add_owner(command: list[str], **kwargs: object):
            nonlocal writer_rejected
            if "--ownerless-validate-cargo-home" in command:
                try:
                    start_unit.run_registry.write_record(
                        second,
                        {
                            "schema_version": 1,
                            "unit": "validate-cargo-owner-interleaving-second.service",
                            "source_checkout": str(self.checkout),
                            "cargo_home": str(cargo_home),
                            "repo": "rrnewton/hermit",
                            "state": "running",
                        },
                    )
                except start_unit.run_registry.RecordLocked:
                    writer_rejected = True
                    raise
            return self.fake(command, **kwargs)

        cleaned, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            start_unit.run_registry.read_record(record),
            record_path=record,
            run=add_owner,
            tool_root=self.root,
        )

        self.assertTrue(writer_rejected)
        self.assertFalse(cleaned)
        self.assertIn("run-population authority", reason or "")
        self.assertTrue(cargo_home.exists())
        self.assertFalse(second.exists())
        self.assertNotIn(
            "cargo_home_removed_at", start_unit.run_registry.read_record(record)
        )

        # Positive control: with no competing owner, the same census/provider
        # path removes and stamps the Cargo home.
        cleaned, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            start_unit.run_registry.read_record(record),
            record_path=record,
            run=self.fake,
            tool_root=self.root,
        )
        self.assertTrue(cleaned, reason)
        self.assertFalse(cargo_home.exists())
        self.assertIsInstance(
            start_unit.run_registry.read_record(record).get(
                "cargo_home_removed_at"
            ),
            str,
        )

    def test_cargo_cleanup_does_not_confirm_dangling_symlink_as_removed(self) -> None:
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "cargo-dangling", temporary=False, current=True
        )

        def leave_dangling(command: list[str], **kwargs: object):
            result = self.fake(command, **kwargs)
            if "--ownerless-validate-cargo-home" in command:
                cargo_home.symlink_to(cargo_home.parent / "missing-cargo-target")
            return result

        cleaned, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            start_unit.run_registry.read_record(record),
            record_path=record,
            run=leave_dangling,
            tool_root=self.root,
        )

        self.assertFalse(cleaned)
        self.assertIn("still exists", reason or "")
        self.assertTrue(cargo_home.is_symlink())
        self.assertNotIn(
            "cargo_home_removed_at", start_unit.run_registry.read_record(record)
        )

    def test_cargo_cleanup_discovers_preexisting_dangling_symlink(self) -> None:
        _checkout, cargo_home, record = self.in_place_cleanup_record(
            "cargo-preexisting-dangling", temporary=False, current=True
        )
        shutil.rmtree(cargo_home)
        cargo_home.symlink_to(cargo_home.parent / "missing-cargo-target")

        cleaned, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            start_unit.run_registry.read_record(record),
            record_path=record,
            run=self.fake,
            tool_root=self.root,
        )

        self.assertFalse(cleaned)
        self.assertIn("still exists", reason or "")
        self.assertTrue(cargo_home.is_symlink())
        self.assertFalse(
            any(
                "--ownerless-validate-cargo-home" in command
                for command in self.fake.commands
            ),
            "a dangling target must be rejected before the provider is invoked",
        )

    def test_sweep_retains_cargo_home_when_wrkslots_recovery_refuses(self) -> None:
        cargo_home = self.root / "ignored/validate/cargo-homes/validate-cargo-refused"
        cargo_home.mkdir(parents=True)
        record_path = self.root / "ignored/validate/runs/validate-cargo-refused.json"
        record = {
            "schema_version": 1,
            "cargo_home": str(cargo_home),
            "state": "completed",
            "exit_code": 0,
            "final_validate_status": "PASSED",
        }
        start_unit.run_registry.write_record(record_path, record)
        self.fake.ownerless_cargo_recovery_rc = 3

        removed, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            record,
            record_path=record_path,
            run=self.fake,
        )

        self.assertFalse(removed)
        self.assertIn("wrkslots retained the Cargo home", reason or "")
        self.assertTrue(cargo_home.is_dir())

    def test_cargo_cleanup_refuses_an_unreadable_handle_census(self) -> None:
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes/validate-cargo-census-unreadable"
        )
        cargo_home.mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-census-owner.json"
        durable = {
            "schema_version": 1,
            "cargo_home": str(cargo_home),
            "state": "completed",
            "exit_code": 0,
            "final_validate_status": "PASSED",
        }
        start_unit.run_registry.write_record(record, durable)
        (record.parent / "validate-unreadable.json").write_text("{")

        removed, reason = start_unit.cleanup_recorded_cargo_home(
            self.root,
            durable,
            record_path=record,
            run=self.fake,
            tool_root=self.root,
        )

        self.assertFalse(removed)
        self.assertIn("cannot read validation handle", reason or "")
        self.assertTrue(cargo_home.exists())
        self.assertFalse(
            any(
                "--ownerless-validate-cargo-home" in command
                for command in self.fake.commands
            )
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
        with contextlib.redirect_stdout(out):
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

    def test_sweep_completed_defers_frozen_checkouts_beyond_batch_limit(self) -> None:
        checkouts: list[Path] = []
        for index in range(start_unit.VALIDATE_CLEANUP_BATCH_LIMIT + 1):
            fresh = (
                start_unit.frozen_checkout_parent(self.root)
                / f"validate-fresh-frozen-batch-{index:02d}"
            )
            (fresh / ".git").mkdir(parents=True)
            checkouts.append(fresh)
            record_path = (
                self.root
                / "ignored/validate/runs"
                / f"validate-frozen-batch-{index:02d}.json"
            )
            start_unit.run_registry.write_record(
                record_path,
                {
                    "schema_version": 1,
                    "unit": f"validate-frozen-batch-{index:02d}.service",
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

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual(
            start_unit.VALIDATE_CLEANUP_BATCH_LIMIT, len(report["removed"])
        )
        self.assertEqual(1, len(report["deferred"]))
        deferred = Path(report["deferred"][0]["checkout"])
        self.assertEqual(
            "deferred by the bounded frozen validation cleanup batch",
            report["deferred"][0]["reason"],
        )
        self.assertIn(deferred, checkouts)
        self.assertTrue(deferred.is_dir())
        self.assertEqual(
            set(checkouts) - {deferred},
            {Path(path) for path in report["removed"]},
        )
        frozen_batch = next(
            command
            for command in self.fake.commands
            if wrkslots_action(command, "recover-ownerless-validate-batch")
        )
        self.assertEqual(
            start_unit.VALIDATE_CLEANUP_BATCH_LIMIT,
            frozen_batch.count("--frozen-validate-checkout"),
        )

    def test_frozen_batch_preserves_nested_recorded_source_checkout(self) -> None:
        frozen = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-nested-source"
        )
        (frozen / ".git").mkdir(parents=True)
        source = self.root / "worktrees/validate/pr2971-source-fixture"
        source.mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-nested-source.json"
        record.parent.mkdir(parents=True, exist_ok=True)
        record.write_text("{}\n")

        outcomes = start_unit.remove_ownerless_checkouts_batch(
            self.root,
            [(frozen, record, source)],
            run=self.fake,
            tool_root=self.root,
            frozen=True,
        )

        self.assertIsNone(outcomes[str(frozen)].reason)
        command = next(
            command
            for command in self.fake.commands
            if wrkslots_action(command, "recover-ownerless-validate-batch")
        )
        self.assertEqual(
            source.relative_to(self.root).as_posix(),
            command[command.index("--repository") + 1],
        )

    def test_sweep_batches_ownerless_checkouts_in_one_census_command(self) -> None:
        first, first_record = self.ownerless_cleanup_record("ownerless-first")
        second, second_record = self.ownerless_cleanup_record("ownerless-second")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(first), str(second)], report["removed"])
        self.assertEqual([], report["retained"])
        batches = [
            command
            for command in self.fake.commands
            if wrkslots_action(command, "recover-ownerless-validate-batch")
        ]
        self.assertEqual(1, len(batches))
        self.assertEqual(2, batches[0].count("--checkout"))
        self.assertEqual(2, batches[0].count("--completed-record"))
        self.assertEqual(2, batches[0].count("--repository"))
        self.assertNotIn("--legacy-validate-checkout", batches[0])
        self.assertFalse(first.exists())
        self.assertFalse(second.exists())
        for record in (first_record, second_record):
            self.assertIn(
                "checkout_removed_at", start_unit.run_registry.read_record(record)
            )

    def test_sweep_holds_ownerless_record_lock_through_batch_removal(self) -> None:
        fresh, record = self.ownerless_cleanup_record("ownerless-locked-through-remove")
        observed_locked = False

        def run(command: list[str], **kwargs: object):
            nonlocal observed_locked
            if wrkslots_action(command, "recover-ownerless-validate-batch"):
                with self.assertRaises(start_unit.run_registry.RecordLocked):
                    with start_unit.run_registry.exclusive_record(
                        record, blocking=False
                    ):
                        pass
                observed_locked = True
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertTrue(observed_locked)
        self.assertEqual([str(fresh)], report["removed"], report)
        self.assertFalse(fresh.exists())

    def test_ownerless_population_authority_survives_runs_replacement(self) -> None:
        fresh, record = self.ownerless_cleanup_record(
            "ownerless-population-parent-replacement"
        )
        self.assert_provider_parent_replacement_is_serialized(
            fresh,
            record,
            action="recover-ownerless-validate-batch",
        )

    def test_ownerless_batch_blocks_contradictory_writer_and_honors_refusal(self) -> None:
        fresh, record = self.ownerless_cleanup_record(
            "ownerless-contended-provider-refusal"
        )
        relative = fresh.relative_to(self.root).as_posix()
        self.fake.ownerless_batch_retained[relative] = "fixture ownership refusal"
        writer_blocked = False

        def run(command: list[str], **kwargs: object):
            nonlocal writer_blocked
            if wrkslots_action(command, "recover-ownerless-validate-batch"):
                with self.assertRaises(start_unit.run_registry.RecordLocked):
                    start_unit.run_registry.update_record(
                        record, blocking=False, exit_code=0
                    )
                writer_blocked = True
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertTrue(writer_blocked)
        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertTrue(
            any("fixture ownership refusal" in row["reason"] for row in report["retained"]),
            report,
        )

    def test_ownerless_sweep_rejects_replaced_run_record_parent(self) -> None:
        fresh, record = self.ownerless_cleanup_record("ownerless-parent-replaced")
        real_exclusive = start_unit.run_registry.exclusive_record
        runs = record.parent
        displaced = runs.with_name("runs.ownerless-displaced")

        @contextlib.contextmanager
        def replace_parent_after_lock(path: Path, *, blocking: bool = True):
            with real_exclusive(path, blocking=blocking):
                if Path(path) == record:
                    os.rename(runs, displaced)
                    shutil.copytree(displaced, runs)
                yield

        with mock.patch.object(
            start_unit.run_registry,
            "exclusive_record",
            side_effect=replace_parent_after_lock,
        ):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertTrue(
            any("parent changed" in row["reason"] for row in report["retained"]),
            report,
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "recover-ownerless-validate-batch")
                for command in self.fake.commands
            )
        )

    def test_ownerless_checkout_refusal_retains_associated_cargo_home(self) -> None:
        fresh, record = self.ownerless_cleanup_record("ownerless-partial-cargo")
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes"
            / "validate-cargo-ownerless-partial"
        )
        cargo_home.mkdir(parents=True)
        durable = start_unit.run_registry.read_record(record)
        durable["cargo_home"] = str(cargo_home)
        start_unit.run_registry.write_record(record, durable)
        self.fake.ownerless_batch_retained[
            fresh.relative_to(self.root).as_posix()
        ] = "fixture ownership refusal"

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual([], report["removed_cargo_homes"])
        self.assertTrue(fresh.exists())
        self.assertTrue(cargo_home.exists())
        self.assertTrue(
            any(
                row["checkout"] == str(cargo_home)
                and "associated validation checkout" in row["reason"]
                for row in report["deferred"]
            ),
            report,
        )

    def test_ownerless_checkout_removal_allows_cargo_home_on_next_sweep(self) -> None:
        fresh, record = self.ownerless_cleanup_record("ownerless-complete-cargo")
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes"
            / "validate-cargo-ownerless-complete"
        )
        cargo_home.mkdir(parents=True)
        durable = start_unit.run_registry.read_record(record)
        durable["cargo_home"] = str(cargo_home)
        start_unit.run_registry.write_record(record, durable)

        first = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        second = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(fresh)], first["removed"], first)
        self.assertEqual([], first["removed_cargo_homes"])
        self.assertFalse(fresh.exists())
        self.assertEqual([str(cargo_home)], second["removed_cargo_homes"], second)
        self.assertFalse(cargo_home.exists())

    def test_ownerless_batch_retains_held_checkout_and_removes_unheld_peer(self) -> None:
        held, held_record = self.ownerless_cleanup_record("ownerless-held")
        unused, unused_record = self.ownerless_cleanup_record("ownerless-unused")
        held_relative = held.relative_to(self.root).as_posix()
        self.fake.ownerless_batch_retained[held_relative] = (
            "live process 4242 uses checkout"
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(unused)], report["removed"])
        self.assertEqual(str(held), report["retained"][0]["checkout"])
        self.assertIn("live process 4242", report["retained"][0]["reason"])
        self.assertTrue(held.exists())
        self.assertFalse(unused.exists())
        self.assertNotIn(
            "checkout_removed_at", start_unit.run_registry.read_record(held_record)
        )
        self.assertIn(
            "checkout_removed_at", start_unit.run_registry.read_record(unused_record)
        )

    def test_invalid_ownerless_batch_report_retains_every_checkout(self) -> None:
        first, _first_record = self.ownerless_cleanup_record("ownerless-malformed-a")
        second, _second_record = self.ownerless_cleanup_record("ownerless-malformed-b")
        self.fake.ownerless_batch_output = json.dumps(
            {
                "schema": 1,
                "batch_limit": 8,
                "process_censuses": 1,
                "shared_process_censuses": 1,
                "same_uid_process_censuses": 3,
                "requested": 2,
                "removed": [
                    {"checkout": first.relative_to(self.root).as_posix()}
                ],
                "retained": [],
            }
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual(2, len(report["retained"]))
        self.assertTrue(
            all(
                "invalid ownerless batch report" in row["reason"]
                for row in report["retained"]
            )
        )
        self.assertTrue(first.exists())
        self.assertTrue(second.exists())

    def test_ownerless_classification_requires_exact_integer_metadata(self) -> None:
        checkout, record = self.ownerless_cleanup_record(
            "classification-integer-metadata"
        )
        relative = checkout.relative_to(self.root).as_posix()
        for field, values in {
            "schema": (True, 1.0, "1"),
            "requested": (True, 1.0, "1"),
            "process_censuses": (True, 1.0, "1"),
        }.items():
            for kind, replacement in zip(
                ("bool", "float", "string"), values, strict=True
            ):
                with self.subTest(field=field, kind=kind):
                    payload: dict[str, object] = {
                        "schema": 1,
                        "requested": 1,
                        "process_censuses": 1,
                        "create_journals": [],
                        "classifications": [
                            {
                                "blocks_entry": False,
                                "checkout": relative,
                                "reason": "terminal validation remains retained",
                                "state": "terminal-retained",
                            }
                        ],
                    }
                    payload[field] = replacement
                    self.fake.ownerless_batch_output = json.dumps(payload)

                    outcomes = start_unit.remove_ownerless_checkouts_batch(
                        self.root,
                        [(checkout, record, self.checkout)],
                        run=self.fake,
                        tool_root=self.root,
                        classify_only=True,
                    )

                    self.assertTrue(outcomes[str(checkout)].blocks_entry)
                    self.assertIn(
                        "invalid ownerless batch report",
                        outcomes[str(checkout)].reason or "",
                    )

    def test_ownerless_classification_requires_state_disposition_pair_and_census(
        self,
    ) -> None:
        checkout, record = self.ownerless_cleanup_record(
            "classification-state-disposition"
        )
        relative = checkout.relative_to(self.root).as_posix()
        mutations = (
            ("could-not-classify", False, 1),
            ("historical-retained", True, 1),
            ("terminal-retained", True, 1),
            ("unknown", False, 1),
            ("terminal-retained", False, 0),
            ("current-incomplete-retained", True, 1),
            ("current-incomplete-retained", False, 0),
            ("current-incomplete-retained", False, 1),  # Not a frozen request.
        )
        for state, blocks_entry, process_censuses in mutations:
            with self.subTest(
                state=state,
                blocks_entry=blocks_entry,
                process_censuses=process_censuses,
            ):
                self.fake.ownerless_batch_output = json.dumps(
                    {
                        "schema": 1,
                        "requested": 1,
                        "process_censuses": process_censuses,
                        "create_journals": [],
                        "classifications": [
                            {
                                "blocks_entry": blocks_entry,
                                "checkout": relative,
                                "reason": "classification mutation",
                                "state": state,
                            }
                        ],
                    }
                )

                outcomes = start_unit.remove_ownerless_checkouts_batch(
                    self.root,
                    [(checkout, record, self.checkout)],
                    run=self.fake,
                    tool_root=self.root,
                    classify_only=True,
                )

                self.assertTrue(outcomes[str(checkout)].blocks_entry)
                self.assertIn(
                    "invalid ownerless batch report",
                    outcomes[str(checkout)].reason or "",
                )

    def test_ownerless_batch_report_requires_typed_entry_disposition(self) -> None:
        checkout, _record = self.ownerless_cleanup_record(
            "ownerless-missing-entry-disposition"
        )
        for disposition in ("missing", None, 0, "unknown"):
            with self.subTest(disposition=disposition):
                retained: dict[str, object] = {
                    "checkout": checkout.relative_to(self.root).as_posix(),
                    "reason": "historical validation remains retained",
                }
                if disposition != "missing":
                    retained["blocks_entry"] = disposition
                self.fake.ownerless_batch_output = json.dumps(
                    {
                        "schema": 2,
                        "batch_limit": 8,
                        "process_censuses": 1,
                        "shared_process_censuses": 1,
                        "same_uid_process_censuses": 0,
                        "requested": 1,
                        "removed": [],
                        "retained": [retained],
                    }
                )

                report = start_unit.sweep_completed_checkouts(
                    self.root, run=self.fake, tool_root=self.root
                )

                self.assertEqual([], report["removed"])
                self.assertEqual(1, len(report["retained"]))
                self.assertTrue(report["retained"][0]["blocks_entry"])
                self.assertIn(
                    "invalid ownerless batch report",
                    report["retained"][0]["reason"],
                )
                self.assertTrue(checkout.exists())

    def test_frozen_nonblocking_report_requires_fresh_same_uid_census(self) -> None:
        checkout = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-no-fresh-census"
        )
        checkout.mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-no-fresh-census.json"
        record.parent.mkdir(parents=True, exist_ok=True)
        record.write_text("{}\n")
        self.fake.ownerless_batch_output = json.dumps(
            {
                "schema": 2,
                "batch_limit": 8,
                "process_censuses": 1,
                "shared_process_censuses": 1,
                "same_uid_process_censuses": 0,
                "requested": 1,
                "removed": [],
                "retained": [
                    {
                        "blocks_entry": False,
                        "checkout": str(checkout),
                        "reason": "historical checkout remains retained",
                    }
                ],
            }
        )

        outcomes = start_unit.remove_ownerless_checkouts_batch(
            self.root,
            [(checkout, record, self.checkout)],
            run=self.fake,
            tool_root=self.root,
            frozen=True,
        )

        self.assertTrue(outcomes[str(checkout)].blocks_entry)
        self.assertIn(
            "invalid ownerless batch report", outcomes[str(checkout)].reason or ""
        )

    def test_ordinary_ownerless_batch_cannot_exempt_entry(self) -> None:
        checkout, record = self.ownerless_cleanup_record(
            "ordinary-false-entry-exemption"
        )
        relative = checkout.relative_to(self.root).as_posix()
        self.fake.ownerless_batch_output = json.dumps(
            {
                "schema": 2,
                "batch_limit": 8,
                "process_censuses": 1,
                "shared_process_censuses": 1,
                "same_uid_process_censuses": 1,
                "requested": 1,
                "removed": [],
                "retained": [
                    {
                        "blocks_entry": False,
                        "checkout": relative,
                        "reason": "ordinary ownerless checkout remains retained",
                    }
                ],
            }
        )

        outcomes = start_unit.remove_ownerless_checkouts_batch(
            self.root,
            [(checkout, record, self.checkout)],
            run=self.fake,
            tool_root=self.root,
        )

        self.assertTrue(outcomes[str(checkout)].blocks_entry)
        self.assertIn(
            "invalid ownerless batch report", outcomes[str(checkout)].reason or ""
        )

    def test_ownerless_batch_does_not_confirm_a_dangling_symlink_as_removed(
        self,
    ) -> None:
        checkout, record = self.ownerless_cleanup_record("ownerless-dangling")
        relative = checkout.relative_to(self.root).as_posix()

        def leave_dangling(command: list[str], **_kwargs: object):
            shutil.rmtree(checkout)
            checkout.symlink_to(checkout.parent / "missing-ownerless-target")
            return completed(
                command,
                stdout=json.dumps(
                    {
                        "schema": 2,
                        "batch_limit": start_unit.VALIDATE_CLEANUP_BATCH_LIMIT,
                        "process_censuses": 1,
                        "shared_process_censuses": 1,
                        "same_uid_process_censuses": 1,
                        "requested": 1,
                        "removed": [{"checkout": relative}],
                        "retained": [],
                    }
                ),
            )

        outcomes = start_unit.remove_ownerless_checkouts_batch(
            self.root,
            [(checkout, record, self.checkout)],
            run=leave_dangling,
            tool_root=self.root,
        )

        self.assertEqual(
            "wrkslots reported success but the checkout still exists",
            outcomes[str(checkout)].reason,
        )
        self.assertTrue(outcomes[str(checkout)].blocks_entry)
        self.assertTrue(checkout.is_symlink())

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

    def test_sweep_retains_record_without_exact_wrkslots_identity(self) -> None:
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
        self.assertTrue(
            any("no exact wrkslots identity" in row["reason"] for row in report["retained"])
        )
        self.assertEqual([], report["bookkeeping_errors"])
        self.assertTrue(fresh.exists())

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
        with contextlib.redirect_stdout(out):
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

    def test_sweep_holds_frozen_record_lock_through_batch_removal(self) -> None:
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-frozen-lock-through-remove"
        )
        (fresh / ".git").mkdir(parents=True)
        self.addCleanup(shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True)
        record = (
            self.root
            / "ignored/validate/runs/validate-frozen-lock-through-remove.json"
        )
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-frozen-lock-through-remove.service",
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
        observed_locked = False

        def run(command: list[str], **kwargs: object):
            nonlocal observed_locked
            if wrkslots_action(command, "recover-ownerless-validate-batch"):
                with self.assertRaises(start_unit.run_registry.RecordLocked):
                    with start_unit.run_registry.exclusive_record(
                        record, blocking=False
                    ):
                        pass
                observed_locked = True
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertTrue(observed_locked)
        self.assertEqual([str(fresh)], report["removed"], report)
        self.assertFalse(fresh.exists())

    def test_frozen_population_authority_survives_runs_replacement(self) -> None:
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-frozen-population-parent-replacement"
        )
        (fresh / ".git").mkdir(parents=True)
        self.addCleanup(
            shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True
        )
        record = (
            self.root
            / "ignored/validate/runs/validate-frozen-population-parent-replacement.json"
        )
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-frozen-population-parent-replacement.service",
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

        self.assert_provider_parent_replacement_is_serialized(
            fresh,
            record,
            action="recover-ownerless-validate-batch",
        )

    def test_frozen_batch_blocks_contradictory_writer_and_honors_refusal(self) -> None:
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-frozen-contended-provider-refusal"
        )
        (fresh / ".git").mkdir(parents=True)
        self.addCleanup(shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True)
        record = (
            self.root
            / "ignored/validate/runs/validate-frozen-contended-provider-refusal.json"
        )
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-frozen-contended-provider-refusal.service",
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
        self.fake.ownerless_batch_retained[str(fresh)] = "fixture ownership refusal"
        writer_blocked = False

        def run(command: list[str], **kwargs: object):
            nonlocal writer_blocked
            if wrkslots_action(command, "recover-ownerless-validate-batch"):
                with self.assertRaises(start_unit.run_registry.RecordLocked):
                    start_unit.run_registry.update_record(
                        record, blocking=False, exit_code=1
                    )
                writer_blocked = True
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertTrue(writer_blocked)
        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertTrue(
            any("fixture ownership refusal" in row["reason"] for row in report["retained"]),
            report,
        )

    def test_frozen_sweep_rejects_replaced_run_record_parent(self) -> None:
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-frozen-parent-replaced"
        )
        (fresh / ".git").mkdir(parents=True)
        self.addCleanup(shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True)
        record = self.root / "ignored/validate/runs/validate-frozen-parent-replaced.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-frozen-parent-replaced.service",
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
        real_exclusive = start_unit.run_registry.exclusive_record
        runs = record.parent
        displaced = runs.with_name("runs.frozen-displaced")

        @contextlib.contextmanager
        def replace_parent_after_lock(path: Path, *, blocking: bool = True):
            with real_exclusive(path, blocking=blocking):
                if Path(path) == record:
                    os.rename(runs, displaced)
                    shutil.copytree(displaced, runs)
                yield

        with mock.patch.object(
            start_unit.run_registry,
            "exclusive_record",
            side_effect=replace_parent_after_lock,
        ):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertTrue(
            any("parent changed" in row["reason"] for row in report["retained"]),
            report,
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "recover-ownerless-validate-batch")
                for command in self.fake.commands
            )
        )

    def test_frozen_checkout_refusal_retains_associated_cargo_home(self) -> None:
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-frozen-partial-cargo"
        )
        (fresh / ".git").mkdir(parents=True)
        self.addCleanup(shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True)
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes"
            / "validate-cargo-frozen-partial"
        )
        cargo_home.mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-frozen-partial.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "unit": "validate-frozen-partial.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "admission": start_unit.FROZEN_RESULT_ADMISSION,
                "cargo_home": str(cargo_home),
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )
        self.fake.ownerless_batch_retained[str(fresh)] = "fixture ownership refusal"

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertEqual([], report["removed_cargo_homes"])
        self.assertTrue(fresh.exists())
        self.assertTrue(cargo_home.exists())
        self.assertTrue(
            any(
                row["checkout"] == str(cargo_home)
                and "associated validation checkout" in row["reason"]
                for row in report["deferred"]
            ),
            report,
        )

    def test_sweep_completed_retains_frozen_checkout_when_use_is_unknown(self) -> None:
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-frozen-held"
        )
        (fresh / ".git").mkdir(parents=True)
        self.fake.fresh = fresh
        record_path = self.root / "ignored/validate/runs/validate-frozen-held.json"
        start_unit.run_registry.write_record(
            record_path,
            {
                "schema_version": 1,
                "unit": "validate-frozen-held.service",
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

        retained_reason = "live process 4242 uses checkout"
        self.fake.ownerless_batch_retained[str(fresh)] = retained_reason
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
        self.assertEqual(retained_reason, report["retained"][0]["reason"])
        self.assertTrue(fresh.is_dir())

    def test_sweep_retains_frozen_checkout_from_managed_location_without_identity(self) -> None:
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
        self.assertTrue(
            any("no exact wrkslots identity" in row["reason"] for row in report["retained"])
        )
        self.assertTrue(fresh.exists())

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
                "wrkslots_slot": fresh.name,
                "wrkslots_generation": 7,
                "repo": "rrnewton/hermit",
                "state": "running",
                # A historical writer once stamped this field while the unit
                # kept running. It is never cleanup authority by itself.
                "finished_at": "2026-09-04T12:01:00+00:00",
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

    def test_cleanup_accepts_collected_current_run_with_typed_admission(self) -> None:
        self.fake.collected_unit = True
        _fresh, record = self.current_unknown_cleanup_record(
            "admitted",
            {
                "state": "admitted",
                "recorded_at": "2026-09-04T12:00:00+00:00",
                "run_number": 7,
            },
        )
        observation = start_unit.terminal_cleanup_unit(
            self.root,
            self.root / "validate-admitted.json",
            record,
            run=self.fake,
        )
        self.assertEqual("validate-admitted", observation.unit)
        self.assertEqual(start_unit.CleanupUnitState.TERMINAL, observation.state)
        self.assertIsNone(observation.reason)
        self.assertEqual("unknown", record["state"])
        self.assertEqual("unknown", record["result"])

    def test_cleanup_retains_unknown_run_when_unit_query_fails(self) -> None:
        _fresh, record = self.current_unknown_cleanup_record(
            "query-failed",
            {
                "state": "admitted",
                "recorded_at": "2026-09-04T12:00:00+00:00",
                "run_number": 7,
            },
        )

        def failed_query(command: list[str], **_kwargs: object):
            return completed(command, rc=1, stderr="systemd unavailable")

        observation = start_unit.terminal_cleanup_unit(
            self.root,
            self.root / "validate-query-failed.json", record, run=failed_query
        )

        self.assertIsNone(observation.unit)
        self.assertEqual(
            start_unit.CleanupUnitState.INDETERMINATE, observation.state
        )
        self.assertEqual(
            "unit state unavailable and run record is not terminal",
            observation.reason,
        )

    def test_cleanup_retains_undisposed_refusal_when_unit_query_fails(self) -> None:
        _fresh, record = self.current_unknown_cleanup_record(
            "refused-query-failed",
            {
                "state": "refused",
                "recorded_at": "2026-09-04T12:00:00+00:00",
                "reason": "stale-base",
                "exit_code": 3,
            },
        )

        def failed_query(command: list[str], **_kwargs: object):
            return completed(command, rc=1, stderr="systemd unavailable")

        observation = start_unit.terminal_cleanup_unit(
            self.root,
            self.root / "validate-refused-query-failed.json",
            record,
            run=failed_query,
        )

        self.assertIsNone(observation.unit)
        self.assertEqual(
            start_unit.CleanupUnitState.INDETERMINATE, observation.state
        )
        self.assertEqual(
            "unit state unavailable and run record is not terminal",
            observation.reason,
        )
        self.assertEqual("unknown", record["state"])
        self.assertEqual("unknown", record["result"])

    def test_cleanup_retains_unknown_run_with_malformed_admission(self) -> None:
        self.fake.collected_unit = True
        _fresh, record = self.current_unknown_cleanup_record(
            "malformed-admission",
            {
                "state": "admitted",
                "recorded_at": "not-a-timestamp",
                "run_number": 7,
            },
        )

        observation = start_unit.terminal_cleanup_unit(
            self.root,
            self.root / "validate-malformed-admission.json",
            record,
            run=self.fake,
        )

        self.assertIsNone(observation.unit)
        self.assertEqual(
            start_unit.CleanupUnitState.INDETERMINATE, observation.state
        )
        self.assertEqual(
            "unit state unavailable and run record is not terminal",
            observation.reason,
        )

    def test_cleanup_retains_running_run_with_typed_admission(self) -> None:
        self.fake.collected_unit = True
        _fresh, record = self.current_unknown_cleanup_record(
            "running",
            {
                "state": "admitted",
                "recorded_at": "2026-09-04T12:00:00+00:00",
                "run_number": 7,
            },
        )
        record["state"] = "running"
        record.pop("result")
        record.pop("detail")
        record.pop("finished_at")

        observation = start_unit.terminal_cleanup_unit(
            self.root,
            self.root / "validate-running.json", record, run=self.fake
        )

        self.assertIsNone(observation.unit)
        self.assertEqual(
            start_unit.CleanupUnitState.INDETERMINATE, observation.state
        )
        self.assertEqual(
            "unit state unavailable and run record is not terminal",
            observation.reason,
        )

    def test_running_unit_vetoes_a_typed_admission_refusal(self) -> None:
        _fresh, record = self.current_unknown_cleanup_record(
            "still-running",
            {
                "state": "refused",
                "recorded_at": "2026-09-04T12:00:00+00:00",
                "reason": "stale-base",
                "exit_code": 3,
            },
        )

        def active_unit(command: list[str], **_kwargs: object):
            return completed(
                command,
                stdout=(
                    "LoadState=loaded\nActiveState=active\nSubState=running\n"
                    "ExecMainCode=0\nExecMainStatus=0\nResult=success\n"
                    "InvocationID=still-running\n"
                ),
            )

        observation = start_unit.terminal_cleanup_unit(
            self.root,
            self.root / "validate-still-running.json", record, run=active_unit
        )

        self.assertIsNone(observation.unit)
        self.assertEqual(start_unit.CleanupUnitState.RUNNING, observation.state)
        self.assertEqual("unit-running", observation.reason)

    def test_sweep_defers_linked_checkout_liveness_to_wrkslots(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-held"
        fresh.mkdir(parents=True)
        self.fake.fresh = fresh
        start_unit.run_registry.write_record(
            self.root / "ignored/validate/runs/validate-held.json",
            {
                "schema_version": 1,
                "unit": "validate-held.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "wrkslots_slot": fresh.name,
                "wrkslots_generation": 7,
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
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
        report = json.loads(out.getvalue())
        self.assertEqual([str(fresh)], report["removed"])
        self.assertEqual([], report["retained"])
        self.assertFalse(fresh.exists())

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
        self.plant_orphan_receipt("signal/result.jsonl")

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
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-test/signal/result.jsonl"
        )
        self.assertTrue(archived.is_file(), "signal cleanup must preserve run evidence")
        self.assert_main_checkout_and_cargo_removed()

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

    def test_materialized_dry_run_reports_one_plan_after_cleanup(self) -> None:
        rc, output, error = self.invoke(
            ["--materialize-target", "--dry-run", "--json"]
        )

        self.assertEqual(0, rc, error)
        lines = output.splitlines()
        self.assertEqual(1, len(lines), output)
        report = json.loads(lines[0])
        self.assertEqual("would-start", report["event"])
        self.assertEqual("planned", report["state"])
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertFalse(self.fake.fresh.exists())
        self.assertEqual([], sorted(self.fake.registered_validate_slots))

    def test_production_fence_allows_same_launch_dry_run_cleanup(self) -> None:
        with mock.patch.object(
            start_unit,
            "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
            PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
        ):
            rc, output, error = self.invoke(
                ["--materialize-target", "--dry-run", "--json"]
            )

        self.assertEqual(0, rc, error)
        self.assertEqual("would-start", json.loads(output)["event"])
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertFalse(self.fake.fresh.exists() or self.fake.fresh.is_symlink())
        self.assertFalse(
            self.private_cargo_home().exists()
            or self.private_cargo_home().is_symlink()
        )
        self.assertTrue(self.runtime_roots)
        self.assertFalse(
            self.runtime_roots[-1].exists() or self.runtime_roots[-1].is_symlink()
        )
        self.assertEqual([], sorted(self.fake.registered_validate_slots))

    def assert_materialized_dry_run_cleanup_observation(
        self,
        *,
        checkout_retained: bool,
        row_retained: bool,
        row_observable: bool = True,
        row_mismatch: str | None = None,
    ) -> None:
        self.fake.wrkslots_remove_rc = 3
        self.fake.wrkslots_remove_stderr = "REFUSED: injected cleanup refusal"
        self.fake.wrkslots_remove_keeps_checkout_on_error = checkout_retained
        self.fake.wrkslots_remove_keeps_row_on_error = row_retained
        if not row_observable:
            self.fake.wrkslots_status_rc = 3
            self.fake.wrkslots_status_stderr = "REFUSED: injected status refusal"
        elif row_mismatch == "generation":
            self.fake.wrkslots_status_generation = self.fake.wrkslots_generation + 1
        elif row_mismatch == "path":
            self.fake.wrkslots_status_path = "worktrees/validate/different-slot"

        rc, output, error = self.invoke(
            ["--materialize-target", "--dry-run", "--json"]
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertEqual("", output)
        self.assertIn("injected cleanup refusal", error)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        path_state = "RETAINED" if checkout_retained else "ABSENT"
        row_state = (
            "RETAINED"
            if row_observable and row_retained and row_mismatch is None
            else "ABSENT"
            if row_observable and not row_retained
            else "COULD-NOT-DETERMINE"
        )
        self.assertIn(f"dry-run checkout path: {path_state}", error)
        self.assertIn(str(self.fake.fresh), error)
        self.assertIn(f"dry-run wrkslots row: {row_state}", error)
        self.assertIn(self.fake.fresh.name, error)
        self.assertIn(str(self.fake.wrkslots_generation), error)
        self.assertEqual(checkout_retained, self.fake.fresh.is_dir())
        self.assertEqual(
            (
                [(self.fake.fresh.name, self.fake.wrkslots_generation)]
                if row_retained
                else []
            ),
            sorted(self.fake.registered_validate_slots),
        )
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))
        self.assertEqual(
            [],
            list((self.root / "worktrees/validate").glob("validate-cargo-*")),
        )
        self.assertTrue(
            (
                self.root
                / "ignored/validate/cargo-homes/validate-cargo-abcd1234"
            ).is_dir()
        )
        self.assertEqual(1, len(self.runtime_roots), self.runtime_roots)
        self.assertTrue(self.runtime_roots[0].is_dir())

    def test_materialized_dry_run_reports_retained_path_and_row(self) -> None:
        self.assert_materialized_dry_run_cleanup_observation(
            checkout_retained=True,
            row_retained=True,
        )

    def test_materialized_dry_run_reports_absent_path_and_retained_row(self) -> None:
        self.assert_materialized_dry_run_cleanup_observation(
            checkout_retained=False,
            row_retained=True,
        )

    def test_materialized_dry_run_reports_retained_path_and_absent_row(self) -> None:
        self.assert_materialized_dry_run_cleanup_observation(
            checkout_retained=True,
            row_retained=False,
        )

    def test_materialized_dry_run_reports_absent_path_and_row(self) -> None:
        self.assert_materialized_dry_run_cleanup_observation(
            checkout_retained=False,
            row_retained=False,
        )

    def test_materialized_dry_run_reports_unavailable_row_observation(self) -> None:
        self.assert_materialized_dry_run_cleanup_observation(
            checkout_retained=True,
            row_retained=True,
            row_observable=False,
        )

    def test_materialized_dry_run_does_not_accept_mismatched_row_path(self) -> None:
        self.assert_materialized_dry_run_cleanup_observation(
            checkout_retained=True,
            row_retained=True,
            row_mismatch="path",
        )

    def test_materialized_dry_run_does_not_accept_mismatched_generation(self) -> None:
        self.assert_materialized_dry_run_cleanup_observation(
            checkout_retained=True,
            row_retained=True,
            row_mismatch="generation",
        )

    def test_service_is_accepted_before_observer_pane_creation(self) -> None:
        def observe_reserved_log(**kwargs: object) -> None:
            log = Path(str(kwargs["log"]))
            self.assertTrue(log.is_file())
            self.assertTrue(
                any(
                    command[0] == "systemd-run" and "validate-lock" in command
                    for command in self.fake.commands
                )
            )
            return None

        with mock.patch.object(
            start_unit.pane_owner,
            "create_pane",
            side_effect=observe_reserved_log,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)

    def test_existing_service_log_is_a_typed_refusal_without_observer(self) -> None:
        log = self.root / "run.log"
        log.write_text("launcher-owned collision\n")

        with mock.patch.object(start_unit.pane_owner, "create_pane") as create_pane:
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn(f"validation log already exists: {log}", error)
        create_pane.assert_not_called()
        self.assertEqual("launcher-owned collision\n", log.read_text())
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("refused", record["state"])
        self.assertEqual("systemd-launch-refused", record["result"])
        self.assertEqual(start_unit.EXIT_REFUSED, record["exit_code"])
        self.assertEqual(f"validation log already exists: {log}", record["detail"])
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            )
        )

    def test_observer_setup_exception_does_not_change_started_run_verdict(self) -> None:
        with mock.patch.object(
            start_unit.pane_owner,
            "create_pane",
            side_effect=RuntimeError("observer fixture refused"),
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc)
        self.assertIn("observer fixture refused", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("completed", record["state"])
        self.assertEqual("success", record["result"])
        self.assertEqual(start_unit.EXIT_PASSED, record["exit_code"])

    def test_running_state_write_failure_retains_accepted_service_resources(self) -> None:
        update_record = start_unit.run_registry.update_record

        def fail_running_state(path: Path, **fields: object) -> dict[str, object]:
            if fields.get("state") == "running":
                raise RuntimeError("injected running-state write failure")
            return update_record(path, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=fail_running_state,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("POST-ACCEPT BOOKKEEPING FAILED", error)
        self.assertIn("RUN CONTINUES independently", error)
        self.assertIn("do not relaunch", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("launching", record["state"])
        self.assertTrue(Path(record["checkout"]).is_dir())
        self.assertTrue(Path(record["cargo_home"]).is_dir())
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        runtime = Path(
            next(value for value in systemd if value.startswith("TMPDIR=")).removeprefix(
                "TMPDIR="
            )
        )
        self.assertTrue(runtime.is_dir())
        self.assertNotIn(str(record["checkout"]), self.fake.removed)

    def test_pane_metadata_write_failure_retains_running_service_resources(self) -> None:
        update_record = start_unit.run_registry.update_record

        def fail_pane_metadata(path: Path, **fields: object) -> dict[str, object]:
            if "pane_id" in fields:
                raise RuntimeError("injected pane-metadata write failure")
            return update_record(path, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=fail_pane_metadata,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("POST-ACCEPT BOOKKEEPING FAILED", error)
        self.assertIn("--attach validate-test", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("running", record["state"])
        self.assertNotIn("pane_id", record)
        self.assertTrue(Path(record["checkout"]).is_dir())
        self.assertTrue(Path(record["cargo_home"]).is_dir())
        self.assertNotIn(str(record["checkout"]), self.fake.removed)

    def test_terminal_record_update_failure_retains_private_resources(self) -> None:
        update_record = start_unit.run_registry.update_record

        def fail_terminal_observation(
            path: Path, *, blocking: bool = True, **fields: object
        ) -> dict[str, object]:
            if "finished_at" in fields:
                raise RuntimeError("injected terminal-observation write failure")
            return update_record(path, blocking=blocking, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=fail_terminal_observation,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("could not record the terminal observation", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertTrue(Path(record["checkout"]).is_dir())
        self.assertTrue(Path(record["cargo_home"]).is_dir())
        self.assertEqual([], self.removed_temporary_paths())

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
        self.assertIn(
            str(self.root / "hermit/scripts/validate.rs"),
            systemd_runs[1],
            "frozen runs need the current front door even when the measured target predates it",
        )
        self.assertNotIn("./scripts/validate.rs", systemd_runs[1])
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
            "the frozen checkout needs refs independent from the dev-hermit worktree family",
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

    def test_typed_stale_admission_has_no_receipt_row_or_publication(self) -> None:
        self.fake.typed_admission_refusal = "stale-base"
        self.fake.final_validate_status = None
        self.fake.executed_nodes = None
        self.fake.executed_tests = None
        self.fake.write_service_result = False
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("typed admission refused", error)
        record_path = self.root / "ignored/validate/runs/validate-test.json"
        record = run_registry.read_record(record_path)
        self.assertEqual("refused", record["admission_result"]["state"])
        self.assertEqual("stale-base", record["admission_result"]["reason"])
        self.assertFalse(start_unit.service_result.result_path(record_path).exists())
        self.assertFalse((self.root / "ledger").exists())
        self.assertFalse(
            any(
                command[0].endswith("ci-hub")
                and command[1:3] == ["validate-status", "--sha"]
                for command in self.fake.commands
            )
        )
        self.assertFalse(
            any(
                command[0].endswith("validate_rows.py")
                or command[0].endswith("series/mirror.py")
                for command in self.fake.commands
            )
        )
        self.assertTrue(
            any(wrkslots_action(command, "remove") for command in self.fake.commands),
            "refused no-evidence checkout must still use liveness-checked cleanup",
        )
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertIn(field, record)
        self.assert_main_checkout_and_cargo_removed()
        self.assertEqual([Path(record["runtime_root"])], self.removed_temporary_paths())

    def test_typed_refusal_retains_private_roots_when_checkout_removal_refuses(self) -> None:
        self.fake.typed_admission_refusal = "stale-base"
        self.fake.final_validate_status = None
        self.fake.executed_nodes = None
        self.fake.executed_tests = None
        self.fake.write_service_result = False
        self.fake.ledger_rows = []
        self.fake.wrkslots_remove_rc = 3
        self.fake.wrkslots_remove_stderr = "fixture ownership refusal"

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("no-evidence checkout was retained", error)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())
        private_cargo = (
            self.root / "ignored/validate/cargo-homes/validate-cargo-abcd1234"
        )
        self.assertTrue(private_cargo.is_dir())
        self.assertNotIn(private_cargo, self.removed_temporary_paths())
        self.assertEqual([], self.removed_temporary_paths())

    def test_typed_refusal_with_completed_payload_retains_all_storage(self) -> None:
        self.fake.typed_admission_refusal = "stale-base"

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("contradicts terminal or product evidence", error)
        record_path = self.root / "ignored/validate/runs/validate-test.json"
        durable = start_unit.run_registry.read_record(record_path)
        self.assertEqual("completed", durable["state"])
        self.assertEqual("PASSED", durable["final_validate_status"])
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                len(command) > 1
                and (
                    command[1].endswith("ci-hub/series/mirror.py")
                    or command[1:3] == ["validate-status", "--sha"]
                )
                for command in self.fake.commands
            )
        )

    # validate-lock's own lines for the eviction of
    # https://github.com/rrnewton/dev-hermit/issues/413, with this fixture's
    # agent and target.
    EVICTION_LOG_LINES = [
        f"REFUSE: validation target {SHA} does not contain freshly fetched "
        "origin/main. Rebase before validating.",
        f"validate-lock: EVICTED agent=test target={SHA} from the queue after "
        "673s: origin/main advanced while it waited, so this run could never "
        "have been admitted. Slot released.",
    ]

    def arrange_typed_eviction(self) -> None:
        self.fake.typed_admission_refusal = "validation-lock-refusal"
        self.fake.inner_log_lines = list(self.EVICTION_LOG_LINES)
        self.fake.actual_exit = 3
        self.fake.final_validate_status = None
        self.fake.executed_nodes = None
        self.fake.executed_tests = None
        self.fake.write_service_result = False
        self.fake.ledger_rows = []

    def invoke_typed_refusal_with_foreign_cleanup_lock(
        self, kind: str, *, holder_seconds: float, wait_seconds: float
    ) -> tuple[int, str, str, subprocess.Popen[str]]:
        """Take a real foreign lock between the refusal's ledger step and cleanup."""

        real_finalize = start_unit.finalize_ledger_mirror
        holder: subprocess.Popen[str] | None = None

        def hold_after_ledger(*args: object, **kwargs: object):
            nonlocal holder
            updated = real_finalize(*args, **kwargs)
            holder = self.foreign_cleanup_lock_holder(kind, seconds=holder_seconds)
            return updated

        with (
            mock.patch.object(
                start_unit,
                "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
                PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
            ),
            mock.patch.object(
                start_unit, "SAME_LAUNCH_CLEANUP_LOCK_WAIT_SECONDS", wait_seconds
            ),
            mock.patch.object(
                start_unit, "SAME_LAUNCH_CLEANUP_LOCK_POLL_SECONDS", 0.005
            ),
            mock.patch.object(
                start_unit, "finalize_ledger_mirror", side_effect=hold_after_ledger
            ),
        ):
            rc, output, error = self.invoke()

        assert holder is not None
        return rc, output, error, holder

    def verdict_lines(self, error: str) -> str:
        """The COULD-NOT-DETERMINE verdict block, without earlier progress lines."""

        start = error.index("validate-run: COULD-NOT-DETERMINE: ")
        return error[start:]

    def test_eviction_with_timed_out_cleanup_names_eviction_and_removal_command(
        self,
    ) -> None:
        """https://github.com/rrnewton/dev-hermit/issues/413 reproduced.

        The cleanup never acquires record authority, so it returns no durable
        record. The durable handle, read again, still carries the refusal, so
        this is the cleanup failure it is, next to the eviction validate-lock
        logged, never a malformed refusal.
        """

        self.arrange_typed_eviction()

        rc, _output, error, holder = self.invoke_typed_refusal_with_foreign_cleanup_lock(
            "generation", holder_seconds=5.0, wait_seconds=0.2
        )

        self.assertIsNone(holder.poll(), "the foreign holder must outlive the wait")
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assertNotIn("malformed", verdict)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): EVICTED -- "
            "origin/main advanced while it waited",
            verdict,
        )
        self.assertIn(f"(run log: validate-lock: EVICTED agent=test target={SHA}", verdict)
        self.assertIn(
            "Separately, storage cleanup did not complete: same-launch cleanup "
            "timed out after 0.2s waiting for run-writer generation authority",
            verdict,
        )
        assert self.fake.fresh is not None
        self.assertIn(
            f"the no-evidence checkout was retained at {self.fake.fresh}", verdict
        )
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertIn(f"private Cargo home retained at {record['cargo_home']}", verdict)
        self.assertIn(f"runtime root retained at {record['runtime_root']}", verdict)
        self.assertIn("remedy: rebase onto the current origin/main", verdict)
        self.assertIn(
            f" remove {self.fake.fresh.name} --validate-complete --coordinator-pid "
            '"$$" --expected-generation 7',
            verdict,
        )
        self.assertIn(
            f"--project-root {self.root.resolve()} --machine "
            f"{start_unit.WRKSLOTS_MACHINE} --allow-existing-unregistered-worktrees",
            verdict,
        )
        self.assertIn(
            f"The retained checkout {self.fake.fresh} holds no result evidence: "
            "validate-lock refused the run before its payload started, the durable "
            "run handle records no product execution or output, no framework result "
            "sidecar exists, and a receipt scan found no untracked *.jsonl. Once no "
            "process uses it, remove it with: ",
            verdict,
        )
        self.assertNotIn("never built", verdict)
        self.assertIn("state:  " + start_unit.TYPED_REFUSAL_STATE, verdict)
        self.assertNotIn("the verdict is unreadable", verdict)
        self.assert_record_storage_retained()
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_eviction_with_complete_cleanup_names_eviction(self) -> None:
        self.arrange_typed_eviction()

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): EVICTED -- "
            "origin/main advanced while it waited in the validate-lock queue",
            verdict,
        )
        self.assertIn("remedy: rebase onto the current origin/main", verdict)
        self.assertNotIn("Separately, storage cleanup did not complete", verdict)
        self.assertNotIn("--validate-complete", verdict)
        self.assertIn("state:  " + start_unit.TYPED_REFUSAL_STATE, verdict)
        self.assert_record_storage_removed()

    def test_other_validation_lock_refusal_quotes_validate_lock(self) -> None:
        self.arrange_typed_eviction()
        # validate_lock.rs prints this when memory admission cannot be read.
        refusal = (
            "REFUSED: validate memory admission is UNKNOWN: memory.max is "
            "unreadable; absence is not permission"
        )
        self.fake.inner_log_lines = [refusal]

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): REFUSED -- the "
            f"validation payload never ran and no verdict exists (run log: {refusal})",
            verdict,
        )
        self.assertNotIn("EVICTED", verdict)
        self.assertIn("remedy: correct the admission condition and retry", verdict)
        self.assert_record_storage_removed()

    def invoke_typed_refusal_around_ledger_step(
        self,
        *,
        before: Callable[[Path], None] | None = None,
        after: Callable[[Path], None] | None = None,
    ) -> tuple[int, str, str]:
        """Change the durable run handle's surroundings around the ledger step."""

        real_finalize = start_unit.finalize_ledger_mirror

        def wrapped(root: Path, tool_root: Path, record_path: Path, **kwargs: object):
            if before is not None:
                before(record_path)
            updated = real_finalize(root, tool_root, record_path, **kwargs)
            if after is not None:
                after(record_path)
            return updated

        with mock.patch.object(start_unit, "finalize_ledger_mirror", side_effect=wrapped):
            return self.invoke()

    def assert_malformed_refusal_without_removal(self, verdict: str, cause: str) -> None:
        self.assertIn(
            "typed admission refusal is malformed, contradicts payload evidence, or "
            f"is not the durable canonical handle: {cause}",
            verdict,
        )
        self.assertIn("checkout, Cargo home, and runtime root retained", verdict)
        self.assertIn(
            "remedy: inspect the durable run handle and framework result sidecar "
            "before retrying",
            verdict,
        )
        self.assertNotIn("EVICTED", verdict)
        self.assertNotIn("--validate-complete", verdict)
        self.assertNotIn("remove it with", verdict)
        self.assertNotIn(start_unit.TYPED_REFUSAL_STATE, verdict)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_eviction_whose_durable_record_breaks_after_the_ledger_step_is_malformed(
        self,
    ) -> None:
        """A cleanup that returns no record proves nothing about the handle.

        Here cleanup acquires record authority and then cannot parse the durable
        record. The refusal read after the wait must not stand in for it: the
        verdict is the malformed one, with no removal command.
        """

        self.arrange_typed_eviction()

        def corrupt(record_path: Path) -> None:
            record_path.write_text('{"not": "a run record"')

        rc, _output, error = self.invoke_typed_refusal_around_ledger_step(after=corrupt)

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assert_malformed_refusal_without_removal(
            verdict,
            "the durable run handle could not be verified: cannot read validation "
            "handle",
        )

    def test_eviction_with_a_result_sidecar_is_a_contradiction_without_removal(
        self,
    ) -> None:
        self.arrange_typed_eviction()
        sidecars: list[Path] = []

        def write_sidecar(record_path: Path) -> None:
            sidecar = start_unit.service_result.result_path(record_path)
            sidecar.write_text("{}\n")
            sidecars.append(sidecar)

        rc, _output, error = self.invoke_typed_refusal_around_ledger_step(
            before=write_sidecar
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assert_malformed_refusal_without_removal(
            verdict, f"a framework result sidecar exists at {sidecars[0]}"
        )
        self.assert_record_storage_retained()

    def test_eviction_with_an_untracked_checkout_receipt_prints_no_removal_command(
        self,
    ) -> None:
        """Cleanup refuses for no-payload authority; the remedy must not delete."""

        self.arrange_typed_eviction()
        self.plant_orphan_receipt("ci/unexpected-receipt.jsonl")

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): EVICTED -- "
            "origin/main advanced while it waited",
            verdict,
        )
        self.assertIn(
            "Separately, storage cleanup did not complete: typed admission refusal "
            "lacks durable no-payload cleanup authority",
            verdict,
        )
        assert self.fake.fresh is not None
        self.assertIn(f"the checkout was retained at {self.fake.fresh}", verdict)
        self.assertNotIn("no-evidence checkout", verdict)
        self.assertIn(
            f"The retained checkout {self.fake.fresh} holds 1 *.jsonl file(s) that "
            "Git does not track and that may be run output: "
            "ci/unexpected-receipt.jsonl; inspect them before removing the checkout",
            verdict,
        )
        self.assertNotIn("--validate-complete", verdict)
        self.assertNotIn("remove it with", verdict)
        self.assertNotIn("never built", verdict)
        self.assertTrue((self.fake.fresh / "ci/unexpected-receipt.jsonl").is_file())
        self.assert_record_storage_retained()

    def test_eviction_with_only_tracked_checkout_jsonl_prints_the_removal_command(
        self,
    ) -> None:
        """https://github.com/rrnewton/dev-hermit/issues/415 withholds cleanup.

        A tracked fixture cannot be output of this run, so the remedy names the
        checked facts and the exact removal command.
        """

        self.arrange_typed_eviction()
        self.plant_orphan_receipt("fixtures/committed.jsonl")
        self.fake.fresh_tracked_jsonl = "fixtures/committed.jsonl\n"

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        assert self.fake.fresh is not None
        self.assertIn(
            f"the no-evidence checkout was retained at {self.fake.fresh}", verdict
        )
        self.assertIn(
            "a receipt scan found no untracked *.jsonl (only 1 *.jsonl file(s) that "
            "Git tracks, which cannot be output of this run; see "
            "https://github.com/rrnewton/dev-hermit/issues/415)",
            verdict,
        )
        self.assertIn("no framework result sidecar exists", verdict)
        self.assertIn(
            f" remove {self.fake.fresh.name} --validate-complete --coordinator-pid "
            '"$$" --expected-generation 7',
            verdict,
        )
        self.assert_record_storage_retained()

    def invoke_eviction_with_a_rebound_durable_handle(
        self, *, receipt: bool, **fields: object
    ) -> tuple[int, str, str]:
        """Point the durable run handle elsewhere after the ledger step.

        With ``receipt``, an untracked receipt sits in this launch's checkout
        throughout. A diagnosis that trusted the rewritten handle would scan
        some other directory, or none, and could then offer to delete this
        checkout.
        """

        self.arrange_typed_eviction()
        if receipt:
            self.plant_orphan_receipt("ci/unexpected-receipt.jsonl")

        def rebind(record_path: Path) -> None:
            record = run_registry.read_record(record_path)
            record.update(fields)
            start_unit.run_registry.write_record(record_path, record)

        return self.invoke_typed_refusal_around_ledger_step(after=rebind)

    def assert_rebound_handle_retains_this_launch(
        self, verdict: str, *, receipt: bool
    ) -> None:
        self.assertNotIn("found no untracked *.jsonl", verdict)
        self.assertNotIn("no-evidence", verdict)
        self.assertNotIn("holds no result evidence", verdict)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertEqual(
            receipt, (self.fake.fresh / "ci/unexpected-receipt.jsonl").is_file()
        )
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        for field in ("cargo_home", "runtime_root"):
            self.assertTrue(Path(record[field]).is_dir(), field)

    def test_eviction_whose_handle_names_another_checkout_offers_no_removal(
        self,
    ) -> None:
        """The handle's checkout must be this launch's before anything vouches.

        The rewritten handle names a clean directory, so a scan of the recorded
        checkout would find nothing and a removal command for this launch's
        checkout, which holds an untracked receipt, would follow.
        """

        assert self.root is not None
        elsewhere = self.root / "elsewhere-clean-checkout"
        elsewhere.mkdir()

        rc, _output, error = self.invoke_eviction_with_a_rebound_durable_handle(
            receipt=True, checkout=str(elsewhere)
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        assert self.fake.fresh is not None
        self.assert_malformed_refusal_without_removal(
            verdict,
            "the durable run handle does not name this launch's storage: durable "
            f"run record checkout {elsewhere} does not match this launch's "
            f"{self.fake.fresh}",
        )
        self.assert_rebound_handle_retains_this_launch(verdict, receipt=True)

    def test_malformed_refusal_after_partial_cleanup_names_only_retained_storage(
        self,
    ) -> None:
        """The verdict names what one cleanup attempt left, not a fixed list.

        Cleanup removes the checkout and then fails on the Cargo home; a result
        sidecar then appears. The checkout is gone, so the verdict must not say
        it was retained.
        """

        self.arrange_typed_eviction()
        real_cleanup = start_unit.cleanup_recorded_run_storage
        sidecars: list[Path] = []

        def cleanup_then_sidecar(root: Path, record_path: Path, **kwargs: object):
            result = real_cleanup(root, record_path, **kwargs)
            sidecar = start_unit.service_result.result_path(record_path)
            sidecar.write_text("{}\n")
            sidecars.append(sidecar)
            return result

        with (
            mock.patch.object(
                start_unit,
                "_cleanup_recorded_cargo_home_impl",
                return_value=(False, "injected Cargo-home failure"),
            ),
            mock.patch.object(
                start_unit,
                "cleanup_recorded_run_storage",
                side_effect=cleanup_then_sidecar,
            ),
        ):
            rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        assert self.fake.fresh is not None
        self.assertFalse(self.fake.fresh.exists(), error)
        self.assertIn(
            "typed admission refusal is malformed, contradicts payload evidence, or "
            "is not the durable canonical handle: a framework result sidecar exists "
            f"at {sidecars[0]}; Cargo home and runtime root retained\n",
            verdict,
        )
        self.assertNotIn("checkout, Cargo home", verdict)
        self.assertNotIn("--validate-complete", verdict)

    def test_eviction_whose_handle_names_another_start_offers_no_removal(
        self,
    ) -> None:
        """A handle from another start cannot vouch for this launch's checkout.

        The checkout is clean here, so only the start-identity binding stands
        between this verdict and a removal command.
        """

        rc, _output, error = self.invoke_eviction_with_a_rebound_durable_handle(
            receipt=False, started_at="2000-01-01T00:00:00+00:00"
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assert_malformed_refusal_without_removal(
            verdict,
            "the durable run handle does not name this launch's storage: durable "
            "run record start identity does not match this launch",
        )
        self.assert_rebound_handle_retains_this_launch(verdict, receipt=False)

    # The exact line `run`'s declared-priority queue prints when `--wait`
    # expires (ci-hub/lib/validate_lock.rs, the `GlobalWait::Refused` return
    # after `started.elapsed() >= Duration::from_secs(args.wait)`).
    QUEUE_TIMEOUT_LINE = (
        f"REFUSED: declared-priority queue did not grant agent=test target={SHA} "
        "within 7200s"
    )

    def test_queue_timeout_refusal_names_the_timeout(self) -> None:
        self.arrange_typed_eviction()
        self.fake.inner_log_lines = [self.QUEUE_TIMEOUT_LINE]

        rc, _output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        verdict = self.verdict_lines(error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): TIMEOUT -- "
            "validate-lock gave up waiting for its turn",
            verdict,
        )
        self.assertIn(f"(run log: {self.QUEUE_TIMEOUT_LINE})", verdict)
        self.assertIn("remedy: queue the same head again", verdict)
        self.assertNotIn("no recognized refusal line", verdict)
        self.assert_record_storage_removed()

    def test_refusal_explanation_without_a_refusal_line_says_so(self) -> None:
        log = self.root / "refusal-without-reason.log"
        log.write_text(
            "validate-lock: waiting in declared-priority queue\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=1.0s exit=3\n"
        )
        silent = self.root / "refusal-without-validate-lock.log"
        silent.write_text("# ci-hub/validate-lock tool COST ACTUAL wall=1.0s exit=3\n")
        reason = run_registry.AdmissionRefusalReason.VALIDATION_LOCK_REFUSAL

        explanation = start_unit.admission_refusal_explanation(reason, log)
        quiet = start_unit.admission_refusal_explanation(reason, silent)
        missing = start_unit.admission_refusal_explanation(
            reason, self.root / "absent.log"
        )

        self.assertEqual("REFUSED", explanation.label)
        self.assertIn(
            f"validate-lock logged no recognized refusal line in {log}; its last "
            "validate-lock line was: validate-lock: waiting in declared-priority "
            "queue",
            explanation.detail,
        )
        self.assertNotIn("logged no refusal line", explanation.detail)
        self.assertIn(
            f"validate-lock logged no recognized refusal line in {silent}; the log "
            "holds no validate-lock line at all",
            quiet.detail,
        )
        self.assertEqual("REFUSED", missing.label)
        self.assertIn("is unreadable", missing.detail)
        stale = start_unit.admission_refusal_explanation(
            run_registry.AdmissionRefusalReason.STALE_BASE, None
        )
        self.assertIn("the run handle names no log", stale.detail)

    def explain_log(self, name: str, *lines: str) -> start_unit.AdmissionRefusalExplanation:
        log = self.root / f"{name}.log"
        log.write_text("".join(f"{line}\n" for line in lines))
        return start_unit.admission_refusal_explanation(
            run_registry.AdmissionRefusalReason.VALIDATION_LOCK_REFUSAL, log
        )

    def test_refusal_explanation_recognizes_a_queue_timeout(self) -> None:
        # validate_lock.rs's `run` queue prints QUEUE_TIMEOUT_LINE when its wait
        # expires; the mid-wait line before it is diagnostic and must not decide.
        unresolved_mid_wait = (
            f"validate-lock: mid-wait freshness UNRESOLVED for target={SHA}, keeping "
            "queue position: validate-lock: current origin/main fetch failed in /x "
            f"(git exit Some(128): boom); freshness for {SHA} is UNRESOLVED."
        )
        explanation = self.explain_log(
            "timeout", unresolved_mid_wait, self.QUEUE_TIMEOUT_LINE
        )

        self.assertEqual("TIMEOUT", explanation.label)
        self.assertIn(f"(run log: {self.QUEUE_TIMEOUT_LINE})", explanation.detail)
        self.assertIn("queue the same head again", explanation.remedy)
        self.assertNotIn("rebase", explanation.remedy)

        # `acquire_ordered`, the owner-watch yield, and a `--no-wait` queue
        # print their own timeout lines.
        for line in (
            "validate-lock: TIMEOUT after 7200s",
            "validate-lock: TIMEOUT after 60s while yielding to owner watch",
            f"REFUSED: declared-priority queue did not grant agent=test target={SHA} "
            "within 0s",
        ):
            with self.subTest(line=line):
                self.assertEqual("TIMEOUT", self.explain_log("timeout-other", line).label)

    def test_refusal_explanation_recognizes_fail_closed_freshness_lines(self) -> None:
        for line in (
            "validate-lock: current origin/main fetch failed in /x (git exit "
            f"Some(128): fatal: unable to access); freshness for {SHA} is UNRESOLVED.",
            "validate-lock: current origin/main fetch could not run in /x (No such "
            f"file or directory); freshness for {SHA} is UNRESOLVED.",
            f"validate-lock: current-main freshness for {SHA} is UNRESOLVED: timed "
            "out after 30s waiting for the shared Git lock",
            f"validate-lock: current-main ancestry for {SHA} is UNRESOLVED in /x "
            "(git exit Some(129): boom).",
            f"validate-lock: validation admission check for {SHA} did not resolve "
            "(exit Some(3)); treating fixed-floor ancestry as STALE.",
            "validate-lock: shared Git directory is UNRESOLVED in /x (boom)",
        ):
            with self.subTest(line=line):
                explanation = self.explain_log("unresolved", line)
                self.assertEqual("UNRESOLVED", explanation.label)
                self.assertIn(f"(run log: {line})", explanation.detail)
                self.assertIn("repair the fetch, network or repository", explanation.remedy)

        # A mid-wait line is not a refusal: it kept the queue position, even
        # though the error it carries names an UNRESOLVED freshness check.
        mid_wait = (
            f"validate-lock: mid-wait freshness UNRESOLVED for target={SHA}, keeping "
            f"queue position: validate-lock: current-main freshness for {SHA} is "
            "UNRESOLVED: boom"
        )
        explanation = self.explain_log("mid-wait", mid_wait)
        self.assertEqual("REFUSED", explanation.label)
        self.assertIn("no recognized refusal line", explanation.detail)
        self.assertIn(f"its last validate-lock line was: {mid_wait}", explanation.detail)

    def test_visibility_failure_does_not_stop_accepted_validation_service(self) -> None:
        self.fake.herdr_status_rc = 1

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc)
        self.assertIn("Herdr server did not become ready", error)
        self.assertIn("continuing WITHOUT an observer pane", error)
        self.assertTrue(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            )
        )

    def materialized_attach_record(self) -> tuple[Path, Path]:
        fresh = self.root / "worktrees/validate/validate-fresh-attach"
        self.fake.fresh = fresh
        self.fake._materialize_fresh_checkout()
        record = self.root / "ignored/validate/runs/validate-test.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "state": "running",
                "unit": "validate-test.service",
                "target": SHA,
                "repo": "rrnewton/hermit",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "materialized_target": True,
                "temporary_checkout": True,
                "wrkslots_slot": fresh.name,
                "wrkslots_generation": self.fake.wrkslots_generation,
                "e2e_result_root": str(self.plant_per_cell_results()),
                "log": str(self.root / "run.log"),
                "started_at": "2099-01-01T00:00:00Z",
            },
        )
        return record, fresh

    def plant_historical_failed_materialized_result(
        self,
        record: Path,
        fresh: Path,
        *,
        final_validate_status: str = "FAILED",
        durable_writeback: bool = True,
        publish_handoff: bool = True,
        handoff_writeback_completed: bool = True,
    ) -> Path | None:
        """Model a schema-3 failed run without promoting it to current authority."""
        exit_code = start_unit.service_result.FINAL_VALIDATE_STATUS_EXIT_CODES[
            final_validate_status
        ]
        self.write_validation_service_schema(
            start_unit.service_result.SELECTION_SCHEMA_VERSION
        )
        source_schema = (
            self.checkout / start_unit.service_result.SCHEMA_RELATIVE_PATH
        )
        target_schema = fresh / start_unit.service_result.SCHEMA_RELATIVE_PATH
        target_schema.parent.mkdir(parents=True, exist_ok=True)
        target_schema.write_text(source_schema.read_text())
        start_unit.service_result.result_path(record).write_text(
            json.dumps(
                {
                    "schema_version": start_unit.service_result.SELECTION_SCHEMA_VERSION,
                    "commit": SHA,
                    "profile": "full",
                    "selection_mode": "full",
                    "final_validate_status": final_validate_status,
                    "exit_code": exit_code,
                    "executed_nodes": 267,
                    "executed_tests": 2816,
                    "scorecard_writeback": {"status": "completed"},
                }
            )
            + "\n"
        )
        handoff = (
            start_unit.publish_scorecard_handoff(
                self.root,
                fresh,
                SHA,
                "validate-test",
                writeback_completed=handoff_writeback_completed,
                expected_files=start_unit.scorecard_writeback_files(fresh),
                wrkslots=start_unit.WrkslotsIdentity(
                    fresh.name, self.fake.wrkslots_generation
                ),
                run=self.fake,
                tool_root=self.root,
            )
            if publish_handoff
            else None
        )
        durable = start_unit.run_registry.read_record(record)
        durable.update(
            state="unknown",
            result="unknown",
            exit_code=exit_code,
            final_validate_status=final_validate_status,
            service_result_schema=start_unit.service_result.SELECTION_SCHEMA_VERSION,
            selection_mode="full",
            executed_nodes=267,
            executed_tests=2816,
            passed_tests=None,
            result_source="validation-service-result",
            detail="historical schema 3 is readable but has no current authority",
        )
        if durable_writeback:
            durable["scorecard_writeback"] = {"status": "completed"}
        if handoff is not None:
            durable["scorecard_handoff"] = str(handoff)
        start_unit.run_registry.write_record(record, durable)
        return handoff

    def test_historical_schema_three_pass_attach_after_sweep_uses_durable_handoff(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        handoff = self.plant_historical_failed_materialized_result(
            record,
            fresh,
            final_validate_status="PASSED",
        )
        self.assertIsNotNone(handoff)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(fresh)], report["removed"])
        self.assertFalse(fresh.exists())
        durable = start_unit.run_registry.read_record(record)
        self.assertIsInstance(durable.get("checkout_removed_at"), str)

        out = io.StringIO()
        err = io.StringIO()
        with (
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
            mock.patch.object(
                start_unit.service_result,
                "pinned_schema_path",
                return_value=(
                    self.checkout / start_unit.service_result.SCHEMA_RELATIVE_PATH
                ),
            ),
        ):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, err.getvalue())
        self.assertIn("already-published output", err.getvalue())
        self.assertNotIn("SCORECARD HANDOFF UNREADABLE", err.getvalue())

    def attach_historical_record(
        self, *, json_output: bool = False
    ) -> tuple[int, str, str]:
        out = io.StringIO()
        err = io.StringIO()
        arguments = ["--attach", "validate-test"]
        if json_output:
            arguments.append("--json")
        with (
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
            mock.patch.object(
                start_unit.service_result,
                "pinned_schema_path",
                return_value=(
                    self.checkout / start_unit.service_result.SCHEMA_RELATIVE_PATH
                ),
            ),
        ):
            rc = start_unit.main(
                arguments,
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )
        return rc, out.getvalue(), err.getvalue()

    def test_historical_attach_after_sweep_refuses_malformed_durable_sidecar(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(
            record,
            fresh,
            final_validate_status="PASSED",
        )
        start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        start_unit.service_result.result_path(record).write_text("not json\n")
        rc, _out, error = self.attach_historical_record()
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("SCORECARD HANDOFF UNREADABLE", error)
        self.assertIn("malformed JSON", error)
        self.assertNotIn("using the already-published output", error)

    def test_historical_attach_after_sweep_refuses_durable_schema_mismatch(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(
            record,
            fresh,
            final_validate_status="PASSED",
        )
        start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        durable = start_unit.run_registry.read_record(record)
        durable["service_result_schema"] = start_unit.service_result.WRITEBACK_SCHEMA_VERSION
        start_unit.run_registry.write_record(record, durable)
        rc, _out, error = self.attach_historical_record()
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("SCORECARD HANDOFF UNREADABLE", error)
        self.assertIn("result carries 3", error)
        self.assertNotIn("using the already-published output", error)

    def test_historical_attach_refuses_absent_checkout_without_removal_record(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(
            record,
            fresh,
            final_validate_status="PASSED",
        )
        shutil.rmtree(fresh)
        rc, _out, error = self.attach_historical_record()
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("SCORECARD HANDOFF UNREADABLE", error)
        self.assertIn("absent without a durable checkout_removed_at", error)
        self.assertNotIn("using the already-published output", error)

    def test_sweep_retains_historical_run_with_malformed_producer_schema(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(
            record,
            fresh,
            final_validate_status="PASSED",
        )
        schema_path = fresh / start_unit.service_result.SCHEMA_RELATIVE_PATH
        schema = json.loads(schema_path.read_text())
        schema["fields"] = ["schema_version"]
        schema_path.write_text(json.dumps(schema))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any(
                "validation-service-result-schema-fields" in row["reason"]
                for row in report["retained"]
            ),
            report["retained"],
        )

    def retired_historical_pass_record(self) -> tuple[Path, Path, Path]:
        record, fresh = self.materialized_attach_record()
        handoff = self.plant_historical_failed_materialized_result(
            record,
            fresh,
            final_validate_status="PASSED",
        )
        self.assertIsNotNone(handoff)
        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        self.assertEqual([str(fresh)], report["removed"])
        self.assertFalse(fresh.exists())
        assert handoff is not None
        return record, fresh, handoff

    def attach_with_bookkeeping_contention(
        self, blocked_field: str
    ) -> tuple[int, str, str]:
        update_record = start_unit.run_registry.update_record

        def contended_update(
            path: Path, *, blocking: bool = True, **fields: object
        ) -> dict[str, object]:
            if blocked_field in fields:
                self.assertFalse(blocking)
                raise start_unit.run_registry.RecordLocked(
                    f"fixture holds {blocked_field} bookkeeping lock"
                )
            return update_record(path, blocking=blocking, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=contended_update,
        ):
            return self.attach_historical_record(json_output=True)

    def test_historical_attach_delivers_validated_when_canonical_bookkeeping_contends(
        self,
    ) -> None:
        _record, fresh, handoff = self.retired_historical_pass_record()
        scorecard_writes = sum(
            command[0].endswith("scorecard.rs") for command in self.fake.commands
        )

        rc, output, error = self.attach_with_bookkeeping_contention(
            "canonical_verdict"
        )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("RUN-RECORD NOT UPDATED", error)
        self.assertIn("already-published output", error)
        self.assertIn(str(handoff), error)
        self.assertNotIn("Traceback", error)
        self.assertNotIn("SCORECARD HANDOFF UNREADABLE", error)
        self.assertEqual(
            scorecard_writes,
            sum(
                command[0].endswith("scorecard.rs") for command in self.fake.commands
            ),
        )
        finished = json.loads(output.splitlines()[-1])
        self.assertEqual("finished", finished["event"])
        self.assertEqual(SHA, finished["target"])
        self.assertEqual(str(fresh), finished["checkout"])
        self.assertEqual("VALIDATED", finished["canonical_verdict"])
        self.assertEqual(start_unit.EXIT_PASSED, finished["wrapper_exit_code"])

    def test_historical_attach_delivers_validated_when_publication_bookkeeping_contends(
        self,
    ) -> None:
        record, fresh, handoff = self.retired_historical_pass_record()
        scorecard_writes = sum(
            command[0].endswith("scorecard.rs") for command in self.fake.commands
        )

        rc, output, error = self.attach_with_bookkeeping_contention(
            "commit_status_publication"
        )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("RUN-RECORD NOT UPDATED", error)
        self.assertIn("already-published output", error)
        self.assertIn(str(handoff), error)
        self.assertNotIn("Traceback", error)
        self.assertNotIn("completed checkout RETAINED", error)
        self.assertEqual(
            scorecard_writes,
            sum(
                command[0].endswith("scorecard.rs") for command in self.fake.commands
            ),
        )
        finished = json.loads(output.splitlines()[-1])
        self.assertEqual("finished", finished["event"])
        self.assertEqual(SHA, finished["target"])
        self.assertEqual(str(fresh), finished["checkout"])
        self.assertEqual("VALIDATED", finished["canonical_verdict"])
        self.assertEqual(
            "published", finished["commit_status_publication"]["state"]
        )
        durable = start_unit.run_registry.read_record(record)
        self.assertNotIn("commit_status_publication", durable)

    def plant_completed_materialized_service_result(self, record: Path) -> None:
        durable = start_unit.run_registry.read_record(record)
        durable["service_result_schema"] = start_unit.service_result.SCHEMA_VERSION
        start_unit.run_registry.write_record(record, durable)
        start_unit.service_result.result_path(record).write_text(
            json.dumps(
                {
                    "schema_version": start_unit.service_result.SCHEMA_VERSION,
                    "commit": SHA,
                    "profile": "full",
                    "selection_mode": "full",
                    "final_validate_status": "PASSED",
                    "detail": None,
                    "exit_code": 0,
                    "executed_nodes": 55,
                    "executed_tests": 862,
                    "passed_tests": 862,
                    "scorecard_writeback": {"status": "completed"},
                }
            )
            + "\n"
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
        self.assertIn("CELL LEDGER PUBLISHED", err.getvalue())
        self.assertFalse(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))
        self.assertEqual(
            1,
            sum(
                len(command) > 1 and command[1] == "publish-commit-status"
                for command in self.fake.commands
            ),
        )
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_attach_terminal_materialized_ledger_is_published_once_then_cleaned(self) -> None:
        record, fresh = self.materialized_attach_record()
        run_registry.update_record(record, github_executable=sys.executable)
        self.fake.scorecard_update = True
        out = io.StringIO()
        err = io.StringIO()

        environment = dict(self.environment)
        environment.pop("FLEET_REAL_GH")
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), mock.patch.dict(os.environ):
            os.environ.pop("FLEET_REAL_GH", None)
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, err.getvalue())
        self.assertFalse(fresh.exists())
        self.assertEqual([str(fresh)], self.fake.removed)
        self.assertTrue(start_unit.cell_ledger_published(run_registry.read_record(record)))
        for command, child_environment in zip(self.fake.commands, self.fake.command_envs):
            if len(command) > 1 and (command[1].endswith("ci-hub/series/mirror.py")
                                     or command[1] == "publish-commit-status"):
                self.assertEqual(sys.executable, child_environment["FLEET_REAL_GH"])
        self.assertFalse(start_unit.scorecard_handoff_path(self.root, "validate-test").exists())
        writer_count = sum(len(c) > 1 and c[1].endswith("ci-hub/series/mirror.py") for c in self.fake.commands)

        second_out = io.StringIO()
        second_err = io.StringIO()
        with contextlib.redirect_stdout(second_out), contextlib.redirect_stderr(
            second_err
        ):
            second_rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_PASSED, second_rc, second_err.getvalue())
        self.assertEqual(
            writer_count,
            sum(len(c) > 1 and c[1].endswith("ci-hub/series/mirror.py") for c in self.fake.commands),
        )
        self.assertTrue(start_unit.cell_ledger_published(run_registry.read_record(record)))

    def test_attach_changed_github_route_retains_verdict_and_unpublished_checkout(self) -> None:
        record, fresh = self.materialized_attach_record()
        run_registry.update_record(record, github_executable=sys.executable)
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), \
                mock.patch.dict(os.environ, {"FLEET_REAL_GH": "/contradictory/gh"}):
            rc = start_unit.main(
                ["--attach", "validate-test"], run=self.fake,
                environment=self.environment, root=self.root, sleep=lambda _seconds: None,
            )
        self.assertEqual(start_unit.EXIT_PASSED, rc, err.getvalue())
        retained = run_registry.read_record(record)
        self.assertEqual("VALIDATED", retained["canonical_verdict"])
        self.assertEqual("failed", retained["ledger_mirror"]["state"])
        self.assertIn("contradicts", retained["ledger_mirror"]["detail"])
        self.assertTrue(fresh.exists())
        self.assertFalse(any(len(c) > 1 and (c[1].endswith("ci-hub/series/mirror.py")
                                             or c[1] == "publish-commit-status")
                             for c in self.fake.commands))

    def test_historical_schema_three_failure_attach_then_sweep_retires_checkout(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        handoff = self.plant_historical_failed_materialized_result(record, fresh)
        self.assertIsNotNone(handoff)
        self.fake.canonical_verdict = "FAILED"
        self.fake.canonical_status_rc = start_unit.EXIT_FAILED
        block_single_remove = True

        def run(command: list[str], **kwargs: object):
            if block_single_remove and wrkslots_action(command, "remove"):
                return completed(
                    command,
                    rc=2,
                    stderr="live observer shell still uses validation checkout",
                )
            return self.fake(command, **kwargs)

        out = io.StringIO()
        err = io.StringIO()
        with (
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
            mock.patch.object(
                start_unit.service_result,
                "pinned_schema_path",
                return_value=fresh / start_unit.service_result.SCHEMA_RELATIVE_PATH,
            ),
        ):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=run,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_FAILED, rc, err.getvalue())
        self.assertTrue(fresh.is_dir())
        self.assertIn("live observer shell", err.getvalue())
        after_attach = start_unit.run_registry.read_record(record)
        self.assertEqual("unknown", after_attach["state"])
        self.assertEqual("unknown", after_attach["result"])
        self.assertEqual("FAILED", after_attach["final_validate_status"])
        self.assertIsNone(after_attach["passed_tests"])
        self.assertNotEqual("VALIDATED", after_attach.get("canonical_verdict"))

        block_single_remove = False
        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertEqual([str(fresh)], report["removed"])
        self.assertEqual([str(handoff)], report["scorecard_handoffs"])
        self.assertFalse(fresh.exists())
        durable = start_unit.run_registry.read_record(record)
        self.assertEqual("unknown", durable["state"])
        self.assertEqual("unknown", durable["result"])
        self.assertEqual("FAILED", durable["final_validate_status"])
        self.assertIsNone(durable["passed_tests"])
        self.assertNotEqual("VALIDATED", durable.get("canonical_verdict"))

    def test_sweep_retains_historical_run_without_recorded_handoff(self) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(
            record, fresh, publish_handoff=False
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        refusals = [
            row
            for row in report["retained"]
            if "already-published verified" in row["reason"]
        ]
        self.assertTrue(refusals, report["retained"])
        self.assertTrue(
            all(row.get("cleanup_failed") is not True for row in refusals)
        )

    def test_sweep_retains_historical_run_with_unverified_handoff(self) -> None:
        record, fresh = self.materialized_attach_record()
        handoff = self.plant_historical_failed_materialized_result(record, fresh)
        self.assertIsNotNone(handoff)
        assert handoff is not None
        (handoff / "SCORECARD.md").write_text("changed after publication\n")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any("failed its digest" in row["reason"] for row in report["retained"]),
            report["retained"],
        )

    def test_sweep_retains_historical_run_with_cells_digest_mismatch(self) -> None:
        record, fresh = self.materialized_attach_record()
        handoff = self.plant_historical_failed_materialized_result(record, fresh)
        self.assertIsNotNone(handoff)
        assert handoff is not None
        (handoff / "ci/compat-envelope/cells.json").write_text(
            '{"changed_after_publication": true}\n'
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any("failed its digest" in row["reason"] for row in report["retained"]),
            report["retained"],
        )

    def test_sweep_retains_historical_run_with_unreadable_result_sidecar(self) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(record, fresh)
        start_unit.service_result.result_path(record).write_text("not json\n")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any("malformed JSON" in row["reason"] for row in report["retained"]),
            report["retained"],
        )

    def test_sweep_retains_historical_run_with_uncertain_writeback(self) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(
            record, fresh, durable_writeback=False
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any("scorecard_writeback" in row["reason"] for row in report["retained"]),
            report["retained"],
        )

    def test_sweep_retains_historical_run_with_incomplete_handoff_writeback(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(
            record, fresh, handoff_writeback_completed=False
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any("handoff_writeback_completed=False" in row["reason"] for row in report["retained"]),
            report["retained"],
        )

    def test_sweep_retains_active_historical_run_with_completed_handoff(self) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_historical_failed_materialized_result(record, fresh)

        def run(command: list[str], **kwargs: object):
            if (
                command[:3] == ["systemctl", "--user", "show"]
                and "validate-test.service" in command
            ):
                return completed(
                    command,
                    stdout=(
                        "LoadState=loaded\nActiveState=active\nSubState=running\n"
                        "InvocationID=historical-active-fixture\n"
                    ),
                )
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any(row["reason"] == "unit-running" for row in report["retained"]),
            report["retained"],
        )

    def test_sweep_retains_inactive_materialized_run_until_writeback_is_recorded(
        self,
    ) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_completed_materialized_service_result(record)
        (fresh / "SCORECARD.md").write_text("scorecard\nupdated-by-inner\n")
        cells = fresh / "ci/compat-envelope/cells.json"
        cells.write_text("{}\nupdated-by-inner\n")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            start_unit.scorecard_handoff_path(self.root, "validate-test").exists()
        )
        self.assertNotIn(
            "scorecard_writeback", start_unit.run_registry.read_record(record)
        )
        self.assertTrue(
            any(
                "completed scorecard_writeback" in row["reason"]
                for row in report["retained"]
            ),
            report["retained"],
        )

    def test_sweep_never_treats_absent_writeback_as_completed(self) -> None:
        fresh, record = self.managed_cleanup_record("materialized-no-writeback")
        durable = start_unit.run_registry.read_record(record)
        durable.update(target=SHA, materialized_target=True)
        start_unit.run_registry.write_record(record, durable)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            start_unit.scorecard_handoff_path(
                self.root, "validate-materialized-no-writeback"
            ).exists()
        )
        self.assertTrue(
            any(
                "scorecard_writeback=None" in row["reason"]
                for row in report["retained"]
            ),
            report["retained"],
        )

    def refused_admission_cleanup_record(
        self,
        suffix: str,
        *,
        state: str = "unknown",
        legacy_ledger_noop: bool = False,
    ) -> tuple[Path, Path]:
        fresh, record = self.managed_cleanup_record(suffix, state=state)
        durable = start_unit.run_registry.read_record(record)
        durable.update(
            kind="validate",
            target=SHA,
            materialized_target=True,
            agent="owner-checkout-watch",
            started_at="2026-08-28T07:00:00+00:00",
            producer=start_unit.run_registry.PRODUCER,
            admission="ci-hub validate-lock",
            pane_role="observer-only",
            parent_checkout_head=SHA,
            validate_lock_child_deadline_seconds=3600,
            e2e_result_root=str(
                self.root / "ignored/validate/artifacts" / suffix / "e2e"
            ),
            safe_ci_dag_runner_log_dir=str(
                self.root / "ignored/validate/artifacts" / suffix / "dag"
            ),
            hermit_run_timeout_seconds=3239,
            log=str(self.root / "ignored/validate" / f"{suffix}.log"),
            cargo_home=str(
                self.root / "ignored/validate/cargo-homes" / f"cargo-{suffix}"
            ),
            service_result_schema=start_unit.service_result.SCHEMA_VERSION,
            result="unknown",
            result_source="validation-service-result",
            detail="validation payload did not start",
            exit_code=None,
            executed_nodes=None,
            executed_tests=None,
            passed_tests=None,
            final_validate_status=None,
            scorecard_writeback=None,
            admission_result={
                "state": "refused",
                "recorded_at": "2026-08-28T07:00:01+00:00",
                "reason": "stale-base",
                "exit_code": 3,
            },
        )
        if legacy_ledger_noop:
            ledger_commit = "b" * 40
            durable["ledger_mirror"] = {
                "commit": ledger_commit,
                "detail": start_unit.LEGACY_ADMISSION_REFUSAL_LEDGER_ERROR,
                "raw_publication": {
                    "attempts": 0,
                    "commit": None,
                    "published": 0,
                    "reaped_retained": 0,
                    "shards": [],
                },
                "recorded_at": "2026-08-28T07:00:02+00:00",
                "repository": start_unit.CELL_LEDGER_REPOSITORY,
                "rows": 90962,
                "scorecard": {
                    "error": start_unit.LEGACY_ADMISSION_REFUSAL_LEDGER_ERROR,
                    "state": "failed",
                },
                "series_commit": ledger_commit,
                "source_commit": ledger_commit,
                "source_repository": start_unit.CELL_LEDGER_REPOSITORY,
                "source_tree": "c" * 40,
                "state": "derived-failed",
            }
        start_unit.run_registry.write_record(record, durable)
        start_unit.run_registry.parse_current_record(durable)
        return fresh, record

    def test_refused_admission_finalizer_records_no_payload_disposition(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-stop-post", state="running"
        )
        before = len(self.fake.commands)
        error = io.StringIO()

        with contextlib.redirect_stderr(error):
            updated = start_unit.finalize_ledger_mirror(
                self.root,
                self.root,
                record,
                record=start_unit.run_registry.read_record(record),
                run=self.fake,
            )

        self.assertTrue(fresh.is_dir())
        self.assertEqual(before, len(self.fake.commands))
        self.assertEqual(
            start_unit.LEDGER_MIRROR_NOT_APPLICABLE,
            updated["ledger_mirror"]["state"],
        )
        self.assertEqual(
            start_unit.ADMISSION_REFUSAL_NO_PAYLOAD_DETAIL,
            updated["ledger_mirror"]["detail"],
        )
        self.assertIn("CELL LEDGER NOT APPLICABLE", error.getvalue())
        self.assertTrue(
            start_unit.cell_ledger_retirement_ready(self.root, record, updated)
        )

    def test_retire_unit_removes_typed_refusal_without_invoking_mirror(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-retire", state="running"
        )

        rc, error = self.retire("admission-refused-retire")

        self.assertEqual(0, rc, error)
        self.assertFalse(fresh.exists())
        self.assertEqual([str(fresh)], self.fake.removed)
        self.assertFalse(
            any(
                len(command) > 1
                and command[1].endswith("ci-hub/series/mirror.py")
                for command in self.fake.commands
            )
        )
        durable = start_unit.run_registry.read_record(record)
        self.assertEqual(
            start_unit.ADMISSION_REFUSAL_NO_PAYLOAD_DETAIL,
            durable["ledger_mirror"]["detail"],
        )

    def test_refused_admission_marker_lock_contention_retains_checkout(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-contended", state="running"
        )
        update_record = start_unit.run_registry.update_record

        def contended_update(
            path: Path, *, blocking: bool = True, **fields: object
        ) -> dict[str, object]:
            if "ledger_mirror" in fields:
                self.assertFalse(blocking)
                raise start_unit.run_registry.RecordLocked(
                    "fixture holds admission refusal marker lock"
                )
            return update_record(path, blocking=blocking, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=contended_update,
        ):
            rc, error = self.retire("admission-refused-contended")

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("RUN-RECORD NOT UPDATED", error)
        self.assertIn("RETIRE-REFUSED", error)
        self.assertTrue(fresh.is_dir())
        self.assertNotIn(
            "ledger_mirror", start_unit.run_registry.read_record(record)
        )
        self.assertFalse(
            any("remove-validate-batch" in command for command in self.fake.commands)
        )

    def test_refused_admission_marker_fsync_failure_retains_checkout(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-fsync", state="running"
        )
        real_fsync = os.fsync

        def refuse_directory_sync(descriptor: int) -> None:
            if stat.S_ISDIR(os.fstat(descriptor).st_mode):
                raise OSError(errno.EIO, "fixture directory sync failure")
            real_fsync(descriptor)

        with mock.patch.object(start_unit.os, "fsync", side_effect=refuse_directory_sync):
            rc, error = self.retire("admission-refused-fsync")

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("not the durable canonical handle", error)
        self.assertIn("direct cell-ledger publication is incomplete", error)
        self.assertIn("RETIRE-REFUSED", error)
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any("remove-validate-batch" in command for command in self.fake.commands)
        )
        self.assertNotIn("ledger_mirror", start_unit.run_registry.read_record(record))

    def test_refused_admission_marker_sync_failure_reports_visible_marker(self) -> None:
        _fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-marker-sync-report", state="running"
        )
        durable = start_unit.run_registry.read_record(record)
        error = io.StringIO()

        with mock.patch.object(
            start_unit,
            "_fsync_directory",
            side_effect=OSError(errno.EIO, "fixture directory sync failure"),
        ), contextlib.redirect_stderr(error):
            returned = start_unit.finalize_ledger_mirror(
                self.root,
                self.root,
                record,
                record=durable,
                run=self.fake,
            )

        persisted = start_unit.run_registry.read_record(record)
        self.assertEqual(
            start_unit.LEDGER_MIRROR_NOT_APPLICABLE,
            persisted["ledger_mirror"]["state"],
        )
        self.assertEqual(persisted["ledger_mirror"], returned["ledger_mirror"])
        self.assertIn("marker is visible in the current namespace", error.getvalue())
        self.assertIn("grants no cleanup authority", error.getvalue())

    def test_attach_typed_refusal_skips_canonical_and_mirror_queries(self) -> None:
        fresh, _record = self.refused_admission_cleanup_record(
            "admission-refused-attach"
        )
        self.fake.fresh = fresh
        output = io.StringIO()
        error = io.StringIO()

        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            rc = start_unit.main(
                ["--attach", "validate-admission-refused-attach"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error.getvalue())
        self.assertFalse(fresh.exists(), error.getvalue())
        self.assertIn("typed admission refused (stale-base)", error.getvalue())
        self.assertFalse(
            any(
                len(command) > 1
                and (
                    command[1].endswith("ci-hub/series/mirror.py")
                    or command[1:3] == ["validate-status", "--sha"]
                )
                for command in self.fake.commands
            )
        )

    ATTACH_CHECKOUT_JSONL = "tests/fixtures/ledger-schema10/legacy/ordinary-only-cells.jsonl"

    def attach_evicted_refusal(
        self,
        suffix: str,
        *,
        checkout_jsonl: bool = False,
        jsonl_tracked: bool = False,
        sidecar: bool = False,
        temporary_checkout: bool = True,
        before_attach: Callable[[Path], None] | None = None,
    ) -> tuple[Path, int, str]:
        fresh, record = self.refused_admission_cleanup_record(suffix)
        durable = start_unit.run_registry.read_record(record)
        durable["admission_result"]["reason"] = "validation-lock-refusal"
        start_unit.run_registry.write_record(record, durable)
        Path(durable["log"]).parent.mkdir(parents=True, exist_ok=True)
        Path(durable["log"]).write_text("\n".join(self.EVICTION_LOG_LINES) + "\n")
        if checkout_jsonl:
            fixture = fresh / self.ATTACH_CHECKOUT_JSONL
            fixture.parent.mkdir(parents=True, exist_ok=True)
            fixture.write_text("{}\n")
            if jsonl_tracked:
                self.fake.fresh_tracked_jsonl = f"{self.ATTACH_CHECKOUT_JSONL}\n"
        if sidecar:
            start_unit.service_result.result_path(record).write_text("{}\n")
        if not temporary_checkout:
            durable = start_unit.run_registry.read_record(record)
            durable["temporary_checkout"] = False
            durable["materialized_target"] = False
            for field in ("wrkslots_slot", "wrkslots_generation"):
                durable.pop(field, None)
            start_unit.run_registry.write_record(record, durable)
        if before_attach is not None:
            before_attach(record)
        self.fake.fresh = fresh
        error = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(error):
            rc = start_unit.main(
                ["--attach", f"validate-{suffix}"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )
        return fresh, rc, error.getvalue()

    def test_attach_eviction_names_eviction(self) -> None:
        fresh, rc, error = self.attach_evicted_refusal("admission-evicted-attach")

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        self.assertFalse(fresh.exists(), error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): EVICTED -- "
            "origin/main advanced while it waited",
            error,
        )
        self.assertIn("remedy: rebase onto the current origin/main", error)
        self.assertIn("state:  " + start_unit.TYPED_REFUSAL_STATE, error)
        self.assertNotIn("Separately", error)

    def test_attach_eviction_with_untracked_checkout_jsonl_names_both(self) -> None:
        """A checkout-local ``*.jsonl`` blocks the durable no-payload marker.

        The verdict names the eviction first, then the retained checkout and the
        file that withheld cleanup. The file is not tracked, so it may be run
        output and no removal command is printed.
        """

        fresh, rc, error = self.attach_evicted_refusal(
            "admission-evicted-attach-jsonl", checkout_jsonl=True
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        self.assertTrue(fresh.is_dir(), error)
        self.assertNotIn("malformed", error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): EVICTED -- "
            "origin/main advanced while it waited",
            error,
        )
        self.assertIn(
            "Separately, cleanup was not attempted because the durable run handle "
            "records no no-payload cell-ledger disposition; and the checkout "
            "receipt check found 1 *.jsonl file(s) (1 not tracked by Git) and "
            "treats any as possible result evidence "
            "(https://github.com/rrnewton/dev-hermit/issues/415): "
            f"{self.ATTACH_CHECKOUT_JSONL}; checkout retained at {fresh} and Cargo "
            "home retained",
            error,
        )
        self.assertIn("remedy: rebase onto the current origin/main", error)
        self.assertIn(
            f"The retained checkout {fresh} holds 1 *.jsonl file(s) that Git does "
            f"not track and that may be run output: {self.ATTACH_CHECKOUT_JSONL}; "
            "inspect them before removing the checkout",
            error,
        )
        self.assertNotIn("--validate-complete", error)
        self.assertIn("state:  " + start_unit.TYPED_REFUSAL_STATE, error)

    def test_attach_eviction_with_only_tracked_checkout_jsonl_prints_removal(
        self,
    ) -> None:
        """Hermit tracks such fixtures, so a real eviction checkout reaches this.

        While https://github.com/rrnewton/dev-hermit/issues/415 is open the
        tracked file still withholds cleanup, but it cannot be run output, so
        the verdict names the retained checkout and the exact removal command.
        """

        fresh, rc, error = self.attach_evicted_refusal(
            "admission-evicted-attach-tracked-jsonl",
            checkout_jsonl=True,
            jsonl_tracked=True,
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        self.assertTrue(fresh.is_dir(), error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): EVICTED", error
        )
        self.assertIn(
            "the checkout receipt check found 1 *.jsonl file(s) (0 not tracked by "
            "Git)",
            error,
        )
        self.assertIn(f"checkout retained at {fresh} and Cargo home retained", error)
        self.assertIn(
            f"The retained checkout {fresh} holds no result evidence: ", error
        )
        self.assertIn(
            f" remove {fresh.name} --validate-complete --coordinator-pid "
            '"$$" --expected-generation 7',
            error,
        )
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_attach_refusal_through_a_symlinked_runs_directory_offers_no_removal(
        self,
    ) -> None:
        """The attach diagnosis repeats the locked check's parent checks.

        The runs directory is reached through a symlink, so the locked check
        cannot bind no-payload authority, and finalization records no
        disposition. The checkout's only ``*.jsonl`` is tracked, so a diagnosis
        that skipped the parent checks would find nothing wrong and print a
        removal command for a checkout named by a non-canonical handle.
        """

        def symlink_runs_directory(record: Path) -> None:
            runs = record.parent
            real = runs.with_name("runs-real")
            runs.rename(real)
            runs.symlink_to(real, target_is_directory=True)

        fresh, rc, error = self.attach_evicted_refusal(
            "admission-evicted-attach-symlinked-runs",
            checkout_jsonl=True,
            jsonl_tracked=True,
            before_attach=symlink_runs_directory,
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        self.assertTrue(fresh.is_dir(), error)
        self.assertTrue((fresh / self.ATTACH_CHECKOUT_JSONL).is_file())
        verdict = error[error.index("validate-run: COULD-NOT-DETERMINE: ") :]
        self.assertIn(
            "typed admission refusal is malformed, contradicts payload evidence, or "
            "is not the durable canonical handle: ",
            verdict,
        )
        self.assertIn("is not canonical", verdict)
        self.assertIn("checkout and Cargo home retained", verdict)
        self.assertNotIn("EVICTED", verdict)
        self.assertNotIn("holds no result evidence", verdict)
        self.assertNotIn("--validate-complete", verdict)
        self.assertNotIn(start_unit.TYPED_REFUSAL_STATE, verdict)
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_attach_in_place_refusal_does_not_call_the_checkout_retained(
        self,
    ) -> None:
        """Cleanup never removes an in-place checkout, so it is not "retained"."""

        fresh, rc, error = self.attach_evicted_refusal(
            "admission-evicted-attach-in-place",
            checkout_jsonl=True,
            temporary_checkout=False,
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        self.assertTrue(fresh.is_dir(), error)
        self.assertIn(
            "typed admission refused (validation-lock-refusal): EVICTED", error
        )
        self.assertIn(
            f"the in-place checkout {fresh} is not this run's to remove; Cargo "
            "home retained",
            error,
        )
        self.assertNotIn(f"checkout retained at {fresh}", error)
        self.assertNotIn("--validate-complete", error)
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_attach_refusal_with_a_result_sidecar_is_a_contradiction(self) -> None:
        fresh, rc, error = self.attach_evicted_refusal(
            "admission-evicted-attach-sidecar", sidecar=True
        )
        sidecar = start_unit.service_result.result_path(
            run_registry.record_path(self.root, "validate-admission-evicted-attach-sidecar")
        )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, error)
        self.assertTrue(fresh.is_dir(), error)
        self.assertIn(
            "typed admission refusal is malformed, contradicts payload evidence, or "
            "is not the durable canonical handle: a framework result sidecar exists "
            f"at {sidecar}; checkout and Cargo home retained",
            error,
        )
        self.assertNotIn("EVICTED", error.split("COULD-NOT-DETERMINE: ", 1)[1])
        self.assertNotIn("Separately", error)
        self.assertNotIn(start_unit.TYPED_REFUSAL_STATE, error)
        self.assertNotIn("--validate-complete", error)
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_attach_refusal_with_published_status_retains_checkout(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-attach-published-status"
        )
        durable = start_unit.run_registry.read_record(record)
        durable["commit_status_publication"] = {
            "state": "published",
            "recorded_at": "2026-08-28T07:00:03+00:00",
            "repository": "rrnewton/hermit",
            "sha": SHA,
            "description": "fixture publication",
            "receipt_commit": "d" * 40,
            "receipt_path": "ignored/validate/receipts/fixture.json",
        }
        start_unit.run_registry.write_record(record, durable)
        self.fake.fresh = fresh
        error = io.StringIO()

        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(error):
            rc = start_unit.main(
                [
                    "--attach",
                    "validate-admission-refused-attach-published-status",
                ],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn(
            "contradicts terminal or product evidence", error.getvalue()
        )
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                len(command) > 1
                and (
                    command[1].endswith("ci-hub/series/mirror.py")
                    or command[1:3] == ["validate-status", "--sha"]
                )
                for command in self.fake.commands
            )
        )

    def test_attach_refusal_marker_lock_contention_retains_checkout(self) -> None:
        fresh, _record = self.refused_admission_cleanup_record(
            "admission-refused-attach-contended"
        )
        self.fake.fresh = fresh
        update_record = start_unit.run_registry.update_record

        def contended_update(
            path: Path, *, blocking: bool = True, **fields: object
        ) -> dict[str, object]:
            if "ledger_mirror" in fields:
                self.assertFalse(blocking)
                raise start_unit.run_registry.RecordLocked("fixture contention")
            return update_record(path, blocking=blocking, **fields)

        with mock.patch.object(
            start_unit.run_registry,
            "update_record",
            side_effect=contended_update,
        ), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(
            io.StringIO()
        ):
            rc = start_unit.main(
                ["--attach", "validate-admission-refused-attach-contended"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_attach_refusal_retains_cargo_when_checkout_removal_refuses(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-attach-partial"
        )
        durable = start_unit.run_registry.read_record(record)
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes"
            / "validate-cargo-admission-refused-attach-partial"
        )
        durable["cargo_home"] = str(cargo_home)
        start_unit.run_registry.write_record(record, durable)
        cargo_home.mkdir(parents=True)
        self.fake.fresh = fresh
        self.fake.wrkslots_remove_rc = 3
        self.fake.wrkslots_remove_stderr = "fixture ownership refusal"
        error = io.StringIO()

        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(error):
            rc = start_unit.main(
                ["--attach", "validate-admission-refused-attach-partial"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertTrue(fresh.is_dir())
        self.assertTrue(cargo_home.is_dir())
        self.assertIn("Cargo home RETAINED", error.getvalue())
        self.assertNotIn(cargo_home, self.removed_temporary_paths())

    def test_attach_retains_cargo_when_checkout_classification_refuses(self) -> None:
        record_path = self.root / "ignored/validate/runs/validate-attach-malformed.json"
        record = {
            "checkout": str(self.root / "missing-malformed-checkout"),
            "cargo_home": str(
                self.root
                / "ignored/validate/cargo-homes/validate-cargo-attach-malformed"
            ),
        }
        error = io.StringIO()

        with mock.patch.object(
            start_unit,
            "cleanup_recorded_checkout",
            return_value=(False, [], "fixture checkout classification refusal"),
        ), mock.patch.object(
            start_unit, "cleanup_recorded_cargo_home"
        ) as cargo_cleanup, contextlib.redirect_stderr(error):
            start_unit.cleanup_after_attach(
                self.root,
                record_path,
                record,
                unit="validate-attach-malformed",
                run=self.fake,
                tool_root=self.root,
            )

        cargo_cleanup.assert_not_called()
        self.assertIn("checkout cleanup did not complete", error.getvalue())

    def test_attach_refusal_removes_cargo_after_checkout_removal(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-attach-complete"
        )
        durable = start_unit.run_registry.read_record(record)
        cargo_home = (
            self.root
            / "ignored/validate/cargo-homes"
            / "validate-cargo-admission-refused-attach-complete"
        )
        durable["cargo_home"] = str(cargo_home)
        start_unit.run_registry.write_record(record, durable)
        cargo_home.mkdir(parents=True)
        self.fake.fresh = fresh

        error = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(
            error
        ):
            rc = start_unit.main(
                ["--attach", "validate-admission-refused-attach-complete"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertFalse(fresh.exists())
        self.assertFalse(cargo_home.exists(), error.getvalue())
        updated = start_unit.run_registry.read_record(record)
        self.assertIsInstance(updated.get("checkout_removed_at"), str)
        self.assertIsInstance(updated.get("cargo_home_removed_at"), str)

    def test_sweep_removes_legacy_refusal_mirror_that_published_zero_rows(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "materialized-admission-refused-legacy-ledger",
            legacy_ledger_noop=True,
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(fresh)], report["removed"], report)
        self.assertFalse(fresh.exists())
        self.assertEqual([], report["scorecard_handoffs"])
        durable = start_unit.run_registry.read_record(record)
        self.assertEqual("derived-failed", durable["ledger_mirror"]["state"])
        self.assertIsInstance(durable.get("checkout_removed_at"), str)

    def test_sweep_retains_legacy_refusal_with_framework_sidecar(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-legacy-sidecar",
            legacy_ledger_noop=True,
        )
        start_unit.service_result.result_path(record).write_text("{}\n")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any(
                row["checkout"] == str(fresh)
                and "cell ledger publication is incomplete" in row["reason"]
                for row in report["retained"]
            ),
            report,
        )
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_sweep_retains_legacy_refusal_from_symlinked_run_handle(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-legacy-symlink",
            legacy_ledger_noop=True,
        )
        external = self.root / "external-refusal-record.json"
        external.write_bytes(record.read_bytes())
        record.unlink()
        record.symlink_to(external)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_dir())
        self.assertTrue(record.is_symlink())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_sweep_retains_legacy_refusal_when_run_handle_is_replaced(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-legacy-replaced",
            legacy_ledger_noop=True,
        )
        real_stat = start_unit.os.stat
        record_observations = 0

        def changing_stat(
            path: os.PathLike[str] | str, *args: object, **kwargs: object
        ) -> os.stat_result:
            nonlocal record_observations
            observed = real_stat(path, *args, **kwargs)
            if Path(path).name == record.name and kwargs.get("dir_fd") is not None:
                record_observations += 1
                if record_observations >= 2:
                    fields = list(observed)
                    fields[1] += 1
                    return os.stat_result(fields)
            return observed

        with mock.patch.object(start_unit.os, "stat", side_effect=changing_stat):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual([], report["removed"], report)
        self.assertGreaterEqual(record_observations, 2)
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_sweep_retains_legacy_refusal_when_run_handle_open_fails(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-legacy-open-error",
            legacy_ledger_noop=True,
        )
        real_open = start_unit.os.open

        def failing_open(
            path: os.PathLike[str] | str, flags: int, *args: object, **kwargs: object
        ) -> int:
            if (
                Path(path).name == record.name
                and kwargs.get("dir_fd") is not None
                and flags & os.O_NOFOLLOW
            ):
                raise OSError(errno.EIO, "fixture read failure", path)
            return real_open(path, flags, *args, **kwargs)

        with mock.patch.object(start_unit.os, "open", side_effect=failing_open):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_sweep_retains_legacy_refusal_while_record_lock_is_held(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-legacy-locked",
            legacy_ledger_noop=True,
        )

        with start_unit.run_registry.exclusive_record(record):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_sweep_holds_refusal_record_lock_through_batch_removal(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-legacy-lock-through-remove",
            legacy_ledger_noop=True,
        )
        observed_locked = False

        def run(command: list[str], **kwargs: object):
            nonlocal observed_locked
            if wrkslots_action(command, "remove-validate-batch"):
                with self.assertRaises(start_unit.run_registry.RecordLocked):
                    with start_unit.run_registry.exclusive_record(
                        record, blocking=False
                    ):
                        pass
                observed_locked = True
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=run, tool_root=self.root
        )

        self.assertTrue(observed_locked)
        self.assertEqual([str(fresh)], report["removed"], report)
        self.assertFalse(fresh.exists())

    def test_sweep_rejects_run_record_parent_replaced_after_lock(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-parent-replaced",
            legacy_ledger_noop=True,
        )
        real_exclusive = start_unit.run_registry.exclusive_record
        runs = record.parent
        displaced = runs.with_name("runs.displaced")
        record_lock_entries = 0
        parent_replaced = False

        @contextlib.contextmanager
        def replace_parent_after_lock(path: Path, *, blocking: bool = True):
            nonlocal record_lock_entries, parent_replaced
            with real_exclusive(path, blocking=blocking):
                if Path(path) == record:
                    record_lock_entries += 1
                    if record_lock_entries == 2:
                        os.rename(runs, displaced)
                        shutil.copytree(displaced, runs)
                        parent_replaced = True
                yield

        with mock.patch.object(
            start_unit.run_registry,
            "exclusive_record",
            side_effect=replace_parent_after_lock,
        ):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )

        self.assertTrue(parent_replaced)
        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertTrue(
            any("parent changed" in row["reason"] for row in report["retained"]),
            report,
        )
        self.assertFalse(
            any(wrkslots_action(command, "remove-validate-batch") for command in self.fake.commands)
        )

    def test_sweep_retains_legacy_refusal_during_owner_watch_correction(self) -> None:
        fresh, _record = self.refused_admission_cleanup_record(
            "admission-refused-owner-watch",
            legacy_ledger_noop=True,
        )
        transaction = start_unit.run_registry.owner_watch_correction_transaction(
            self.root
        )
        transaction.write_text("fixture correction in progress\n")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_sweep_retains_legacy_refusal_with_any_outer_exit(self) -> None:
        for index, value in enumerate((0, 1, 137)):
            with self.subTest(exit_code=value):
                fresh, record = self.refused_admission_cleanup_record(
                    f"admission-refused-outer-exit-{index}",
                    legacy_ledger_noop=True,
                )
                durable = start_unit.run_registry.read_record(record)
                durable["exit_code"] = value
                start_unit.run_registry.write_record(record, durable)

                report = start_unit.sweep_completed_checkouts(
                    self.root, run=self.fake, tool_root=self.root
                )

                self.assertNotIn(str(fresh), report["removed"], report)
                self.assertTrue(fresh.is_dir())
        self.assertFalse(
            any(wrkslots_action(command, "remove") for command in self.fake.commands)
        )

    def test_sweep_retains_dangling_run_handle_symlink(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "admission-refused-legacy-dangling",
            legacy_ledger_noop=True,
        )
        record.unlink()
        record.symlink_to(self.root / "missing-external-record.json")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_dir())
        self.assertTrue(record.is_symlink())

    def test_legacy_refusal_mirror_near_misses_remain_retained(self) -> None:
        cases = (
            "attempted",
            "attempted-bool",
            "published",
            "shard",
            "raw-commit",
            "wrong-detail",
            "wrong-scorecard",
            "different-ledger-heads",
            "reaped-retained",
            "extra-publication-field",
            "published-ledger",
            "unchanged-ledger",
            "published-ledger-extra-field",
            "payload-result",
            "outer-exit-zero",
            "outer-exit-one",
            "outer-exit-signal",
            "commit-status",
            "scorecard-writeback",
            "scorecard-files",
            "scorecard-handoff",
            "qualifying-receipt",
            "malformed-qualifying-receipt",
            "archived-receipt",
            "malformed-archived-receipt",
            "service-result-sidecar",
            "broken-sidecar-symlink",
            "admitted",
            "wrong-refusal-code",
        )
        for index, case in enumerate(cases):
            with self.subTest(case=case):
                fresh, record = self.refused_admission_cleanup_record(
                    f"legacy-refusal-near-miss-{index}",
                    legacy_ledger_noop=True,
                )
                durable = start_unit.run_registry.read_record(record)
                publication = durable["ledger_mirror"]
                if case == "attempted":
                    publication["raw_publication"]["attempts"] = 1
                elif case == "attempted-bool":
                    publication["raw_publication"]["attempts"] = False
                elif case == "published":
                    publication["raw_publication"]["published"] = 1
                elif case == "shard":
                    publication["raw_publication"]["shards"] = ["2026-08.jsonl"]
                elif case == "raw-commit":
                    publication["raw_publication"]["commit"] = "d" * 40
                elif case == "wrong-detail":
                    publication["detail"] = "different derived failure"
                elif case == "wrong-scorecard":
                    publication["scorecard"]["error"] = "different derived failure"
                elif case == "different-ledger-heads":
                    publication["series_commit"] = "d" * 40
                elif case == "reaped-retained":
                    publication["raw_publication"]["reaped_retained"] = 1
                elif case == "extra-publication-field":
                    publication["unexpected"] = "must refuse"
                elif case in {
                    "published-ledger",
                    "unchanged-ledger",
                    "published-ledger-extra-field",
                }:
                    publication = publication_report(SHA)
                    publication["state"] = (
                        "unchanged" if case == "unchanged-ledger" else "published"
                    )
                    publication["recorded_at"] = "2026-08-28T07:00:02+00:00"
                    if case == "published-ledger-extra-field":
                        publication["unexpected"] = "must refuse"
                    durable["ledger_mirror"] = publication
                elif case == "payload-result":
                    durable.update(
                        result="failure",
                        exit_code=1,
                        executed_nodes=1,
                        executed_tests=1,
                        passed_tests=0,
                        final_validate_status="FAILED",
                    )
                elif case == "outer-exit-zero":
                    durable["exit_code"] = 0
                elif case == "outer-exit-one":
                    durable["exit_code"] = 1
                elif case == "outer-exit-signal":
                    durable["exit_code"] = 137
                elif case == "commit-status":
                    durable["commit_status_publication"] = {"state": "failed"}
                elif case == "scorecard-writeback":
                    durable["scorecard_writeback"] = {"status": "completed"}
                elif case == "scorecard-files":
                    durable["scorecard_writeback_files"] = []
                elif case == "scorecard-handoff":
                    durable["scorecard_handoff"] = str(
                        self.root / "ignored/validate/scorecard-handoffs/forged"
                    )
                elif case == "qualifying-receipt":
                    durable["qualifying_receipt"] = True
                elif case == "malformed-qualifying-receipt":
                    durable["qualifying_receipt"] = []
                elif case == "archived-receipt":
                    durable["archived_orphaned_receipts"] = [
                        str(self.root / "ignored/validate/orphaned-receipts/result.jsonl")
                    ]
                elif case == "malformed-archived-receipt":
                    durable["archived_orphaned_receipts"] = "result.jsonl"
                elif case == "service-result-sidecar":
                    start_unit.service_result.result_path(record).write_text("{}\n")
                elif case == "broken-sidecar-symlink":
                    start_unit.service_result.result_path(record).symlink_to("missing")
                elif case == "admitted":
                    durable["admission_result"] = {
                        "state": "admitted",
                        "recorded_at": "2026-08-28T07:00:01+00:00",
                        "run_number": 1,
                    }
                elif case == "wrong-refusal-code":
                    durable["admission_result"]["exit_code"] = 0
                record.write_text(json.dumps(durable, sort_keys=True) + "\n")

                self.assertFalse(
                    start_unit.cell_ledger_retirement_ready(
                        self.root, record, durable
                    )
                )
                self.assertTrue(fresh.is_dir())

    def test_legacy_refusal_with_checkout_local_jsonl_remains_retained(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "legacy-refusal-local-jsonl", legacy_ledger_noop=True
        )
        (fresh / "result.jsonl").write_text('{"result":"evidence"}\n')

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertFalse(
            start_unit.cell_ledger_retirement_ready(
                self.root, record, start_unit.run_registry.read_record(record)
            )
        )

    def test_legacy_refusal_with_unreadable_checkout_scan_remains_retained(self) -> None:
        fresh, record = self.refused_admission_cleanup_record(
            "legacy-refusal-unreadable-checkout", legacy_ledger_noop=True
        )

        with mock.patch.object(
            start_unit.os,
            "walk",
            side_effect=OSError(errno.EACCES, "fixture checkout scan failure"),
        ):
            report = start_unit.sweep_completed_checkouts(
                self.root, run=self.fake, tool_root=self.root
            )
            retirement_ready = start_unit.cell_ledger_retirement_ready(
                self.root, record, start_unit.run_registry.read_record(record)
            )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.exists())
        self.assertFalse(retirement_ready)

    def test_sweep_removes_materialized_checkout_after_refused_admission(self) -> None:
        fresh, record = self.managed_cleanup_record(
            "materialized-admission-refused", state="unknown"
        )
        durable = start_unit.run_registry.read_record(record)
        durable.update(
            kind="validate",
            target=SHA,
            materialized_target=True,
            agent="owner-checkout-watch",
            started_at="2026-08-28T07:00:00+00:00",
            producer=start_unit.run_registry.PRODUCER,
            admission="ci-hub validate-lock",
            pane_role="observer-only",
            parent_checkout_head=None,
            validate_lock_child_deadline_seconds=3600,
            e2e_result_root=str(
                self.root
                / "ignored/validate/artifacts/materialized-admission-refused/e2e"
            ),
            safe_ci_dag_runner_log_dir=str(
                self.root
                / "ignored/validate/artifacts/materialized-admission-refused/dag"
            ),
            hermit_run_timeout_seconds=3239,
            log=str(self.root / "ignored/validate/materialized-admission-refused.log"),
            cargo_home=str(
                self.root
                / "ignored/validate/cargo-homes/validate-cargo-admission-refused"
            ),
            result="unknown",
            detail="validation payload did not start",
            exit_code=None,
            final_validate_status=None,
            admission_result={
                "state": "refused",
                "recorded_at": "2026-08-28T07:00:01+00:00",
                "reason": "stale-base",
                "exit_code": 3,
            },
        )
        start_unit.run_registry.write_record(record, durable)
        durable = start_unit.finalize_ledger_mirror(
            self.root,
            self.root,
            record,
            record=durable,
            run=self.fake,
        )
        self.assertEqual(
            start_unit.LEDGER_MIRROR_NOT_APPLICABLE,
            durable["ledger_mirror"]["state"],
        )

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(fresh)], report["removed"], report)
        self.assertFalse(fresh.exists())
        self.assertEqual([], report["scorecard_handoffs"])
        self.assertFalse(
            start_unit.scorecard_handoff_path(
                self.root, "validate-materialized-admission-refused"
            ).exists()
        )
        self.assertIsInstance(
            start_unit.run_registry.read_record(record).get("checkout_removed_at"),
            str,
        )

    def test_sweep_retains_refused_admission_with_incomplete_cell_ledger(self) -> None:
        fresh, record = self.managed_cleanup_record(
            "materialized-admission-refused-ledger", state="unknown"
        )
        durable = start_unit.run_registry.read_record(record)
        durable.update(
            kind="validate",
            target=SHA,
            materialized_target=True,
            agent="owner-checkout-watch",
            started_at="2026-08-28T07:00:00+00:00",
            producer=start_unit.run_registry.PRODUCER,
            admission="ci-hub validate-lock",
            pane_role="observer-only",
            parent_checkout_head=None,
            validate_lock_child_deadline_seconds=3600,
            e2e_result_root=str(
                self.root
                / "ignored/validate/artifacts/materialized-admission-refused-ledger/e2e"
            ),
            safe_ci_dag_runner_log_dir=str(
                self.root
                / "ignored/validate/artifacts/materialized-admission-refused-ledger/dag"
            ),
            hermit_run_timeout_seconds=3239,
            log=str(
                self.root
                / "ignored/validate/materialized-admission-refused-ledger.log"
            ),
            cargo_home=str(
                self.root
                / "ignored/validate/cargo-homes/validate-cargo-admission-refused-ledger"
            ),
            result="unknown",
            detail="validation payload did not start",
            exit_code=None,
            final_validate_status=None,
            admission_result={
                "state": "refused",
                "recorded_at": "2026-08-28T07:00:01+00:00",
                "reason": "stale-base",
                "exit_code": 3,
            },
            ledger_mirror={
                "state": "failed",
                "recorded_at": "2026-08-28T07:00:02+00:00",
                "detail": "contradictory fixture must remain fail-closed",
                "repository": start_unit.CELL_LEDGER_REPOSITORY,
            },
        )
        start_unit.run_registry.write_record(record, durable)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any(
                "cell ledger publication is incomplete" in row["reason"]
                for row in report["retained"]
            ),
            report["retained"],
        )

    def test_attach_after_inactive_sweep_retains_completion_then_publishes_ledger(self) -> None:
        record, fresh = self.materialized_attach_record()
        self.plant_completed_materialized_service_result(record)
        self.fake.scorecard_update = True

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())

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
        self.assertFalse(fresh.exists())
        durable = start_unit.run_registry.read_record(record)
        self.assertEqual(
            {"status": "completed"}, durable["scorecard_writeback"]
        )
        self.assertTrue(start_unit.cell_ledger_published(durable))
        self.assertNotIn("scorecard_handoff", durable)
        self.assertFalse(start_unit.scorecard_handoff_path(self.root, "validate-test").exists())


    def test_attach_error_cleans_materialized_checkout_after_archiving_receipt(self) -> None:
        _record, fresh = self.materialized_attach_record()
        receipt = fresh / "result.jsonl"
        receipt_payload = '{"result":"orphaned"}\n'
        receipt.write_text(receipt_payload)
        old_row = self.fake.default_ledger_row()
        old_row["started_at"] = "2000-01-01T00:00:01Z"
        old_row["finished_at"] = "2000-01-01T00:00:02Z"
        self.fake.ledger_rows = [json.dumps(old_row)]
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

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, err.getvalue())
        self.assertFalse(fresh.exists())
        self.assertEqual([str(fresh)], self.fake.removed)
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-test/result.jsonl"
        )
        self.assertEqual(receipt_payload, archived.read_text())
        self.assertIn("CANONICAL-VERDICT-UNAVAILABLE", err.getvalue())

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

    def test_materialize_target_preserves_real_caller_git_state_on_success(self) -> None:
        target = self.initialize_real_materialize_source()
        before = self.caller_git_state()
        self.assertEqual("caller-branch\n", before["branch"])
        self.assertEqual("", before["status"])
        self.assertNotEqual("", before["index"])
        self.assertEqual("", before["cached_diff"])
        self.assertNotEqual(target, str(before["head"]).strip())

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ):
            rc, _output, error = self.invoke(
                ["--materialize-target", "--", "full"], target=target
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertEqual(before, self.caller_git_state())
        exact_checkout = Path(self._working_directory())
        self.assertEqual(self.root / "worktrees/validate", exact_checkout.parent)
        self.assertEqual(target, self.fake.systemd_checkout_head)
        self.assertFalse(exact_checkout.exists())
        self.assert_main_checkout_and_cargo_removed(exact_checkout)
        self.assertTrue(start_unit.cell_ledger_published(run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json")))
        self.assertFalse(start_unit.scorecard_handoff_path(self.root, "validate-test").exists())
        self.assertEqual(
            "caller head\n", (self.checkout / "materialized-version").read_text()
        )

    def test_full_start_unit_path_ignores_hostile_git_dir_for_head(self) -> None:
        source_head, target, hostile = self.initialize_hostile_git_redirect(
            dirty_source=False
        )
        self.assertNotEqual(source_head, target)

        rc, _output, error = self.invoke(target=target, environment=hostile)

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn(f"checkout HEAD is {source_head}", error)
        self.assertFalse(any(wrkslots_action(command, "create") for command in self.fake.commands))
        for child in self.fake.command_envs:
            self.assertIsNotNone(child)
            assert child is not None
            self.assertTrue(start_unit.git_env.GIT_REPOSITORY_ENV.isdisjoint(child))

    def test_materialize_path_ignores_hostile_git_index_for_cleanliness(self) -> None:
        source_head, target, hostile = self.initialize_hostile_git_redirect(
            dirty_source=True
        )
        self.assertNotEqual(source_head, target)
        actual_status = self.git_output(
            self.checkout, "status", "--porcelain=v1"
        )
        self.assertIn("SCORECARD.md", actual_status)
        hostile_status = subprocess.run(
            ["git", "-C", str(self.checkout), "status", "--porcelain=v1"],
            check=True,
            capture_output=True,
            text=True,
            env=hostile,
        ).stdout
        self.assertEqual("", hostile_status)
        before_head = self.git_output(self.checkout, "rev-parse", "HEAD^{commit}")
        before_status = self.git_output(
            self.checkout, "status", "--porcelain=v1", "--untracked-files=all"
        )
        before_index = self.git_output(self.checkout, "ls-files", "--stage")
        before_worktree = self.worktree_bytes(self.checkout)

        rc, _output, error = self.invoke(
            ["--materialize-target"], target=target, environment=hostile
        )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertEqual(target, self.fake.systemd_checkout_head)
        self.assertEqual(
            before_head, self.git_output(self.checkout, "rev-parse", "HEAD^{commit}")
        )
        self.assertEqual(
            before_status,
            self.git_output(
                self.checkout, "status", "--porcelain=v1", "--untracked-files=all"
            ),
        )
        self.assertEqual(
            before_index, self.git_output(self.checkout, "ls-files", "--stage")
        )
        self.assertEqual(before_worktree, self.worktree_bytes(self.checkout))
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        for child in self.fake.command_envs:
            self.assertIsNotNone(child)
            assert child is not None
            self.assertTrue(start_unit.git_env.GIT_REPOSITORY_ENV.isdisjoint(child))

    def test_materialize_target_preserves_real_caller_git_state_on_launcher_refusal(
        self,
    ) -> None:
        target = self.initialize_real_materialize_source()
        self.fake.systemd_launch_rc = 1
        self.fake.systemd_launch_stderr = "systemd-run: exact launcher refusal\n"
        before = self.caller_git_state()
        self.assertEqual("caller-branch\n", before["branch"])
        self.assertEqual("", before["status"])
        self.assertNotEqual("", before["index"])
        self.assertEqual("", before["cached_diff"])

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ):
            rc, _output, error = self.invoke(
                ["--materialize-target", "--", "full"], target=target
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn(self.fake.systemd_launch_stderr, error)
        self.assertEqual(before, self.caller_git_state())
        self.assertEqual(target, self.fake.systemd_checkout_head)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertEqual(self.root / "worktrees/validate", self.fake.fresh.parent)
        self.assertFalse(self.fake.fresh.exists())

    def test_materialize_target_uses_registered_exact_checkout_without_moving_source(self) -> None:
        self.fake.source_head = "d" * 40
        before = self.worktree_bytes(self.checkout)

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        exact_checkout = Path(self._working_directory())
        self.assertEqual(self.root / "worktrees/validate", exact_checkout.parent)
        self.assertNotEqual(self.checkout, exact_checkout)
        create = next(
            command
            for command in self.fake.commands
            if wrkslots_action(command, "create")
        )
        self.assertIn(f"checkout={SHA}", create)
        self.assertFalse(
            any(
                command[:3] == ["git", "-C", str(self.checkout)]
                and "checkout" in command[3:]
                for command in self.fake.commands
            )
        )
        self.assertEqual("d" * 40, self.fake.source_head)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(str(self.checkout), record["source_checkout"])
        self.assertEqual(str(exact_checkout), record["checkout"])
        self.assertTrue(record["materialized_target"])
        self.assertNotIn("branch", record)
        self.assert_main_checkout_and_cargo_removed(exact_checkout)
        self.assertFalse(exact_checkout.exists())
        self.assertTrue(start_unit.cell_ledger_published(run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json")))
        self.assertFalse(start_unit.scorecard_handoff_path(self.root, "validate-test").exists())

    def test_materialized_ledger_commit_is_recorded_before_cleanup(self) -> None:
        self.fake.source_head = "d" * 40
        before = self.worktree_bytes(self.checkout)
        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.assert_main_checkout_and_cargo_removed(exact_checkout)
        self.assertFalse(exact_checkout.exists())
        self.assertFalse(start_unit.scorecard_handoff_path(self.root, "validate-test").exists())
        record = run_registry.read_record(self.root / "ignored/validate/runs/validate-test.json")
        self.assertTrue(start_unit.cell_ledger_published(record))
        self.assertEqual(SHA, record["ledger_mirror"]["scorecard"]["hermit_commit"])
        self.assertNotIn("scorecard_handoff", record)
        self.assertIn("CELL LEDGER PUBLISHED", error)
        sweep = start_unit.sweep_completed_checkouts(self.root, run=self.fake, tool_root=self.root)
        self.assertEqual([], sweep["removed"])

    def test_materialized_source_edit_after_publication_refuses_cleanup(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_tamper_on_status = "SCORECARD.md"
        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertIn("authored work", error)
        record = run_registry.read_record(self.root / "ignored/validate/runs/validate-test.json")
        self.assertTrue(start_unit.cell_ledger_published(record))
        self.assertNotIn("scorecard_writeback_files", record)

    def test_materialized_scorecard_reused_generation_refuses_and_retains(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        self.fake.wrkslots_status_generation = self.fake.wrkslots_generation + 1

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertTrue(exact_checkout.is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertFalse(
            start_unit.scorecard_handoff_path(self.root, "validate-test").exists()
        )
        self.assertIn("no longer matches its recorded live wrkslots", error)

    def test_materialized_scorecard_replaced_slot_path_refuses_and_retains(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        self.fake.wrkslots_status_path = "worktrees/validate/reused-elsewhere"

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertTrue(exact_checkout.is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertIn("no longer matches its recorded live wrkslots", error)

    def test_materialized_scorecard_moved_head_refuses_before_publication(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        self.fake.scorecard_head_after_write = "e" * 40

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertTrue(exact_checkout.is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertFalse(
            any(wrkslots_action(command, "status") for command in self.fake.commands)
        )
        self.assertIn("source checkout HEAD changed", error)
        self.assertIn("CELL LEDGER PUBLICATION INCOMPLETE", error)

    def test_scorecard_handoff_refuses_a_missing_pair_member(self) -> None:
        fresh, _record = self.managed_cleanup_record("pair")
        self.fake.fresh = fresh
        (fresh / "SCORECARD.md").write_text("scorecard\n")
        cells = fresh / "ci/compat-envelope/cells.json"
        cells.parent.mkdir(parents=True)
        cells.write_text("{}\n")
        handoff = start_unit.publish_scorecard_handoff(
            self.root,
            fresh,
            SHA,
            "validate-pair",
            writeback_completed=True,
            expected_files=start_unit.scorecard_writeback_files(fresh),
            wrkslots=start_unit.WrkslotsIdentity(fresh.name, 7),
            run=self.fake,
            tool_root=self.root,
        )
        (handoff / "SCORECARD.md").unlink()

        with self.assertRaisesRegex(RuntimeError, "invalid file set"):
            start_unit.read_scorecard_handoff(
                self.root, str(handoff), SHA, "validate-pair"
            )

    def test_scorecard_handoff_refuses_missing_cells_member(self) -> None:
        unit = "validate-pair-cells"
        fresh, _record = self.managed_cleanup_record("pair-cells")
        self.fake.fresh = fresh
        (fresh / "SCORECARD.md").write_text("scorecard\n")
        cells = fresh / "ci/compat-envelope/cells.json"
        cells.parent.mkdir(parents=True)
        cells.write_text("{}\n")
        handoff = start_unit.publish_scorecard_handoff(
            self.root,
            fresh,
            SHA,
            unit,
            writeback_completed=True,
            expected_files=start_unit.scorecard_writeback_files(fresh),
            wrkslots=start_unit.WrkslotsIdentity(fresh.name, 7),
            run=self.fake,
            tool_root=self.root,
        )
        (handoff / "ci/compat-envelope/cells.json").unlink()

        with self.assertRaisesRegex(RuntimeError, "invalid file set"):
            start_unit.read_scorecard_handoff(
                self.root, str(handoff), SHA, unit
            )

    def test_old_handoff_file_is_preserved_without_overriding_ledger_delivery(self) -> None:
        self.fake.source_head = "d" * 40
        handoff = start_unit.scorecard_handoff_path(self.root, "validate-test")
        handoff.parent.mkdir(parents=True)
        handoff.write_text("old handoff evidence\n")
        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertEqual("old handoff evidence\n", handoff.read_text())
        self.assertFalse(self.fake.fresh.exists())
        self.assert_main_checkout_and_cargo_removed()
        self.assertTrue(start_unit.cell_ledger_published(run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json")))

    def test_sweep_registry_read_waits_for_a_contended_state_lock(self) -> None:
        """Preserve the original PR lock-wait behavioral control."""
        self.managed_cleanup_record("wait-lock-flag")

        start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        status_commands = [
            command
            for command in self.fake.commands
            if wrkslots_action(command, "status")
        ]
        self.assertEqual(1, len(status_commands), status_commands)
        command = status_commands[0]
        self.assertIn("--wait-lock", command)
        self.assertEqual(
            str(start_unit.WRKSLOTS_STATUS_LOCK_WAIT_SECONDS),
            command[command.index("--wait-lock") + 1],
        )
        # The wait must be a real one. A zero wait is the defect wearing the
        # flag, and would satisfy a presence-only assertion.
        self.assertGreater(start_unit.WRKSLOTS_STATUS_LOCK_WAIT_SECONDS, 0)


    def test_a_state_lock_that_clears_within_the_wait_does_not_refuse_the_batch(
        self,
    ) -> None:
        """Preserve the original PR lock-wait behavioral control."""
        fresh, _record_path = self.managed_cleanup_record("lock-clears")
        inner = self.fake

        def contended(command: list[str], **kwargs: object):
            if wrkslots_action(command, "status"):
                waited = (
                    "--wait-lock" in command
                    and float(command[command.index("--wait-lock") + 1]) > 0
                )
                if not waited:
                    inner.wrkslots_status_rc = 3
                    inner.wrkslots_status_stderr = (
                        "REFUSED: state lock is busy for "
                        f"{self.root}/worktrees/ACTIVE; retry after the current "
                        "command exits"
                    )
                else:
                    inner.wrkslots_status_rc = 0
                    inner.wrkslots_status_stderr = ""
            return inner(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=contended, tool_root=self.root
        )

        self.assertEqual([str(fresh)], report["removed"], report)
        self.assertEqual([], report["retained"], report)


    def test_a_registry_read_failure_that_is_not_the_lock_still_refuses(
        self,
    ) -> None:
        """Preserve the original PR lock-wait behavioral control."""
        fresh, _record_path = self.managed_cleanup_record("registry-unreadable")
        self.fake.wrkslots_status_rc = 3
        self.fake.wrkslots_status_stderr = "REFUSED: registry storage is inconsistent"

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"], report)
        self.assertTrue(fresh.is_dir())
        reasons = [row["reason"] for row in report["retained"]]
        self.assertTrue(
            any("registry storage is inconsistent" in reason for reason in reasons),
            reasons,
        )
        self.assertTrue(
            any("30s state-lock wait limit" in reason for reason in reasons),
            reasons,
        )


    def test_real_registry_lock_wait_refusal_release_and_deadline(self) -> None:
        # Use the recorded provider with an empty, isolated project. No real
        # registrations, validation roots, or cleanup operations are involved.
        tool = Path(__file__).resolve().parents[2] / "bin/wrkslots"
        project = self.root / "lock-project"
        project.mkdir()
        (project / "liveness.py").write_text("raise SystemExit(2)\n")
        environment = {
            key: value for key, value in os.environ.items()
            if not key.startswith("WRKSLOTS_")
        }
        provider = tool.parents[2] / "hermit/agent-utils/py/wrkslots/__main__.py"
        initialized = subprocess.run(
            [sys.executable, str(provider), "--machine", "lock-test", "init", str(project),
             "--liveness-command", "liveness.py"],
            env=environment, text=True, capture_output=True, timeout=10,
        )
        self.assertEqual(0, initialized.returncode, initialized.stderr)
        command = [str(tool), "--project-root", str(project), "--machine", "lock-test"]
        unlocked = subprocess.run(
            [*command, "status", "--format", "json"],
            env=environment, text=True, capture_output=True, timeout=10,
        )
        self.assertEqual(0, unlocked.returncode, unlocked.stderr)
        snapshot = json.loads(unlocked.stdout)
        self.assertEqual(str(project), snapshot["project_root"])
        self.assertEqual([], snapshot["active"])
        lock = Path(snapshot["control_dir"]) / "ACTIVE.global.lock"
        self.assertTrue(lock.is_relative_to(project))
        # Bind the actual interpreter PID for the wait observation. The normal
        # wrapper above proves status transport; PATH's python3 may be a launcher.
        command = [sys.executable, str(provider), *command[1:]]
        with lock.open("r+") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            for limit in (0.0, 0.25):
                started = time.monotonic()
                refused = subprocess.run(
                    [*command, "--wait-lock", str(limit), "status", "--format", "json"],
                    env=environment, text=True, capture_output=True, timeout=10,
                )
                elapsed = time.monotonic() - started
                self.assertEqual(3, refused.returncode, refused.stdout + refused.stderr)
                self.assertIn("state lock is busy", refused.stderr)
                self.assertGreaterEqual(elapsed, limit)
                print(f"registry-lock-control held limit={limit} exit={refused.returncode} elapsed={elapsed:.6f}")
            started = time.monotonic()
            child = subprocess.Popen(
                [*command, "--wait-lock", "3", "status", "--format", "json"],
                env=environment, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            try:
                deadline = time.monotonic() + 5
                waiting = False
                observations = set()
                while child.poll() is None and time.monotonic() < deadline:
                    process = Path(f"/proc/{child.pid}")
                    descriptors = list((process / "fd").iterdir())
                    self.assertLess(len(descriptors), 256)
                    lock_open = False
                    for descriptor in descriptors:
                        try:
                            lock_open |= descriptor.readlink() == lock
                        except FileNotFoundError:
                            pass
                    wchan = (process / "wchan").read_text().strip()
                    observations.add((lock_open, wchan))
                    if lock_open and wchan == "hrtimer_nanosleep":
                        waiting = True
                        break
                    time.sleep(0.01)
                self.assertTrue(waiting, f"status did not observably wait on ACTIVE.global.lock; observations={sorted(observations)!r}, exit={child.poll()!r}")
                self.assertIsNone(child.poll(), "status exited while the lock was still held")
                print(f"registry-lock-control observed-wait pid={child.pid} lock={lock}")
                fcntl.flock(held, fcntl.LOCK_UN)
                stdout, stderr = child.communicate(timeout=10)
                self.assertEqual(0, child.returncode, stderr)
                self.assertEqual(snapshot, json.loads(stdout))
                print(f"registry-lock-control released limit=3 exit={child.returncode} elapsed={time.monotonic() - started:.6f}")
            finally:
                if child.poll() is None:
                    child.kill()
                child.communicate(timeout=10)

    def test_sweep_caches_registry_refusals_and_requeries_on_next_sweep(self) -> None:
        checkouts = [self.managed_cleanup_record(f"registry-{n}")[0] for n in range(3)]
        failures = (
            (3, "", "REFUSED: state lock is busy", "30s state-lock wait limit"),
            (3, "", "configuration is unsafe", "configuration is unsafe"),
            (0, "not JSON", "", "invalid JSON"),
            (0, "[]", "", "did not identify the exact project"),
            (0, json.dumps({"schema": 1, "project_root": str(self.root), "active": []}), "", "did not identify the exact project"),
            (0, json.dumps({"schema": 2, "project_root": "/wrong-root", "active": []}), "", "did not identify the exact project"),
            (0, json.dumps({"schema": 2, "project_root": str(self.root), "active": {}}), "", "did not identify the exact project"),
        )
        for rc, output, diagnostic, expected in failures:
            with self.subTest(output=output, diagnostic=diagnostic):
                self.fake.commands.clear()
                self.fake.wrkslots_status_rc = rc
                self.fake.wrkslots_status_output = output
                self.fake.wrkslots_status_stderr = diagnostic
                report = start_unit.sweep_completed_checkouts(
                    self.root, run=self.fake, tool_root=self.root
                )
                commands = [cmd for cmd in self.fake.commands if wrkslots_action(cmd, "status")]
                self.assertEqual(1, len(commands))
                self.assertEqual("30.0", commands[0][commands[0].index("--wait-lock") + 1])
                self.assertEqual([], report["removed"])
                self.assertEqual([], report["removed_cargo_homes"])
                self.assertEqual([], report["scorecard_handoffs"])
                self.assertEqual(sorted(map(str, checkouts)), sorted(row["checkout"] for row in report["retained"]))
                self.assertEqual(1, len({row["reason"] for row in report["retained"]}))
                for row in report["retained"]:
                    self.assertIn(expected, row["reason"])
                    if diagnostic:
                        self.assertIn(diagnostic, row["reason"])
                    self.assertNotIn("after waiting", row["reason"])
                self.assertTrue(all(path.exists() for path in checkouts))
                self.assertFalse(any(wrkslots_action(cmd, "remove-validate-batch") for cmd in self.fake.commands))
                self.assertEqual([], self.fake.removed)
        self.fake.commands.clear()
        self.fake.wrkslots_status_rc = 0
        self.fake.wrkslots_status_output = None
        self.fake.wrkslots_status_stderr = ""
        recovered = start_unit.sweep_completed_checkouts(self.root, run=self.fake, tool_root=self.root)
        self.assertEqual(sorted(map(str, checkouts)), sorted(recovered["removed"]))
        self.assertEqual([], recovered["retained"])
        self.assertEqual(1, sum(wrkslots_action(cmd, "status") for cmd in self.fake.commands))
        self.assertFalse(any(path.exists() for path in checkouts))

    def test_sweep_caches_registry_command_launch_failure(self) -> None:
        checkouts = [self.managed_cleanup_record(f"launch-error-{n}")[0] for n in range(3)]
        calls = []

        def run(command, **kwargs):
            if wrkslots_action(command, "status"):
                calls.append(command)
                raise OSError("registry executable unavailable")
            return self.fake(command, **kwargs)

        for _ in range(2):
            report = start_unit.sweep_completed_checkouts(self.root, run=run, tool_root=self.root)
            self.assertEqual([], report["removed"])
            self.assertEqual(sorted(map(str, checkouts)), sorted(row["checkout"] for row in report["retained"]))
            self.assertEqual({"registry executable unavailable"}, {row["reason"] for row in report["retained"]})
            self.assertTrue(all(path.exists() for path in checkouts))
        self.assertEqual(2, len(calls))
        self.assertEqual([], self.fake.removed)

    def test_sweep_hands_off_pre_fix_materialized_scorecard_before_removal(self) -> None:
        fresh, record_path = self.managed_cleanup_record("materialized-scorecard")
        record = start_unit.run_registry.read_record(record_path)
        record.update(
            target=SHA,
            materialized_target=True,
            scorecard_writeback={"status": "completed"},
        )
        (fresh / "SCORECARD.md").write_text("scorecard\nupdated\n")
        cells = fresh / "ci/compat-envelope/cells.json"
        cells.parent.mkdir(parents=True)
        cells.write_text("{}\nupdated\n")
        record["scorecard_writeback_files"] = start_unit.scorecard_writeback_files(
            fresh
        )
        start_unit.run_registry.write_record(record_path, record)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        handoff = start_unit.scorecard_handoff_path(
            self.root, "validate-materialized-scorecard"
        )
        self.assertEqual([str(fresh)], report["removed"])
        self.assertEqual([str(handoff)], report["scorecard_handoffs"])
        self.assertFalse(fresh.exists())
        self.assertEqual("scorecard\nupdated\n", (handoff / "SCORECARD.md").read_text())
        self.assertEqual("{}\nupdated\n", (handoff / "ci/compat-envelope/cells.json").read_text())
        durable = start_unit.run_registry.read_record(record_path)
        self.assertEqual(str(handoff), durable["scorecard_handoff"])
        status_commands = [
            command
            for command in self.fake.commands
            if wrkslots_action(command, "status")
        ]
        self.assertEqual(1, len(status_commands))
        self.assertNotIn("--slot", status_commands[0])

    def test_sweep_shares_one_registration_snapshot_before_the_removal_batch(
        self,
    ) -> None:
        checkouts: list[Path] = []
        for suffix in ("materialized-one", "materialized-two"):
            fresh, record_path = self.managed_cleanup_record(suffix)
            record = start_unit.run_registry.read_record(record_path)
            record.update(
                target=SHA,
                materialized_target=True,
                scorecard_writeback={"status": "completed"},
            )
            (fresh / "SCORECARD.md").write_text(f"{suffix}\n")
            cells = fresh / "ci/compat-envelope/cells.json"
            cells.parent.mkdir(parents=True)
            cells.write_text("{}\n")
            record["scorecard_writeback_files"] = (
                start_unit.scorecard_writeback_files(fresh)
            )
            start_unit.run_registry.write_record(record_path, record)
            checkouts.append(fresh)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual(sorted(map(str, checkouts)), sorted(report["removed"]))
        status_commands = [
            command
            for command in self.fake.commands
            if wrkslots_action(command, "status")
        ]
        self.assertEqual(1, len(status_commands))
        self.assertNotIn("--slot", status_commands[0])
        removal_commands = [
            command
            for command in self.fake.commands
            if wrkslots_action(command, "remove-validate-batch")
        ]
        self.assertEqual(1, len(removal_commands))
        self.assertEqual(2, removal_commands[0].count("--slot"))

    def test_sweep_retains_materialized_checkout_without_writer_file_identities(
        self,
    ) -> None:
        fresh, record_path = self.managed_cleanup_record(
            "materialized-no-writer-identities"
        )
        record = start_unit.run_registry.read_record(record_path)
        record.update(
            target=SHA,
            materialized_target=True,
            scorecard_writeback={"status": "completed"},
        )
        start_unit.run_registry.write_record(record_path, record)
        (fresh / "SCORECARD.md").write_text("updated\n")
        cells = fresh / "ci/compat-envelope/cells.json"
        cells.parent.mkdir(parents=True)
        cells.write_text("{}\n")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any(
                "scorecard_writeback_files" in row["reason"]
                for row in report["retained"]
            ),
            report["retained"],
        )

    def test_sweep_retains_materialized_checkout_when_handoff_conflicts(self) -> None:
        fresh, record_path = self.managed_cleanup_record("materialized-conflict")
        record = start_unit.run_registry.read_record(record_path)
        record.update(
            target=SHA,
            materialized_target=True,
            scorecard_writeback={"status": "completed"},
        )
        (fresh / "SCORECARD.md").write_text("updated\n")
        cells = fresh / "ci/compat-envelope/cells.json"
        cells.parent.mkdir(parents=True)
        cells.write_text("{}\n")
        record["scorecard_writeback_files"] = start_unit.scorecard_writeback_files(
            fresh
        )
        start_unit.run_registry.write_record(record_path, record)
        handoff = start_unit.scorecard_handoff_path(
            self.root, "validate-materialized-conflict"
        )
        handoff.parent.mkdir(parents=True)
        handoff.write_text("conflict\n")

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([], report["removed"])
        self.assertTrue(fresh.is_dir())
        self.assertTrue(
            any("scorecard handoff" in row["reason"] for row in report["retained"])
        )

    def test_ordinary_mode_still_refuses_source_head_different_from_target(self) -> None:
        self.fake.source_head = "d" * 40

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn(f"checkout HEAD is {'d' * 40}", error)
        self.assertFalse(any(wrkslots_action(command, "create") for command in self.fake.commands))

    def test_in_place_mode_still_refuses_a_dirty_source_repository(self) -> None:
        self.fake.dirty = " M ci/compat-envelope/cells.json\n"

        rc, _output, error = self.invoke(["--in-place", "--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("checkout is dirty", error)
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )

    def test_materialize_target_refuses_missing_or_mismatched_target(self) -> None:
        self.fake.source_head = "d" * 40
        for target_rc, resolved, expected in (
            (1, SHA, "cannot resolve requested target"),
            (0, "e" * 40, "requested target resolved"),
        ):
            with self.subTest(target_rc=target_rc):
                self.fake.commands.clear()
                self.fake.source_target_rc = target_rc
                self.fake.source_target = resolved

                rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

                self.assertEqual(start_unit.EXIT_REFUSED, rc)
                self.assertIn(expected, error)
                self.assertFalse(
                    any(wrkslots_action(command, "create") for command in self.fake.commands)
                )

    def test_materialize_target_cleans_registered_checkout_if_head_is_wrong(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.fresh_head = "e" * 40

        with mock.patch.object(
            start_unit,
            "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
            PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
        ):
            rc, _output, error = self.invoke(
                ["--materialize-target", "--", "full"]
            )

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("fresh checkout resolved", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_materialize_target_uses_exact_target_despite_dirty_stale_source(
        self,
    ) -> None:
        self.fake.source_head = "d" * 40
        cells = self.checkout / "ci/compat-envelope/cells.json"
        cells.write_text('{"uncommitted": true}\n')
        (self.checkout / "unrelated.txt").write_text("caller work\n")
        self.fake.dirty = (
            " M ci/compat-envelope/cells.json\n"
            "?? unrelated.txt\n"
        )
        before = self.worktree_bytes(self.checkout)

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertEqual(SHA, self.fake.fresh_head)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(SHA, record["target"])
        self.assertNotIn("branch", record)

    def test_materialize_target_never_reads_source_head_or_branch(self) -> None:
        cases = (
            ("unborn-head", 0, 128),
            ("broken-branch", 128, 0),
            ("detached-unborn", 1, 128),
        )
        for suffix, branch_rc, head_rc in cases:
            with self.subTest(suffix=suffix):
                self.fake.commands.clear()
                self.fake.source_branch_rc = branch_rc
                self.fake.source_head_rc = head_rc
                unit = f"validate-source-{suffix}"

                rc, _output, error = self.invoke(
                    ["--materialize-target", "--", "full"], unit=unit
                )

                self.assertEqual(start_unit.EXIT_PASSED, rc, error)
                source_commands = [
                    command
                    for command in self.fake.commands
                    if command[:3] == ["git", "-C", str(self.checkout)]
                ]
                self.assertFalse(
                    any(command[3] == "symbolic-ref" for command in source_commands)
                )
                self.assertFalse(
                    any(command[-1] == "HEAD^{commit}" for command in source_commands)
                )
                self.assertTrue(
                    any(command[-1] == f"{SHA}^{{commit}}" for command in source_commands)
                )
                # FakeRun deterministically reuses one Cargo path, unlike
                # production mktemp.  Remove the completed fixture handle so
                # the next subcase does not manufacture a second historical
                # owner for that reused path.
                run_registry.record_path(self.root, unit).unlink()
                self.fake.fresh = None

    def test_materialize_target_refuses_dirty_fresh_checkout(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.fresh_dirty = " M scripts/validate.rs\n"

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("checkout is dirty", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_two_materialized_publications_preserve_caller_work_and_distinct_targets(
        self,
    ) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        cells = self.checkout / "ci/compat-envelope/cells.json"
        cells.write_text('{"uncommitted": true}\n')
        self.fake.dirty = " M ci/compat-envelope/cells.json\n"
        before = self.worktree_bytes(self.checkout)

        first_unit = "validate-integration-first"
        rc, _output, error = self.invoke(
            ["--materialize-target", "--", "full"], unit=first_unit
        )
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        first_handoff = start_unit.scorecard_handoff_path(self.root, first_unit)
        self.assertFalse(first_handoff.exists())
        first_record_path = self.root / "ignored/validate/runs" / f"{first_unit}.json"
        first_record = run_registry.read_record(first_record_path)
        self.assertTrue(start_unit.cell_ledger_published(first_record))
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.fake.fresh = None

        second_target = "e" * 40
        self.fake.target = second_target
        self.fake.source_target = second_target
        self.fake.fresh_head = second_target
        second_unit = "validate-integration-second"
        rc, _output, error = self.invoke(
            ["--materialize-target", "--", "full"],
            target=second_target,
            unit=second_unit,
        )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertEqual(second_target, self.fake.fresh_head)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.assertEqual(first_record, run_registry.read_record(first_record_path))
        second_handoff = start_unit.scorecard_handoff_path(self.root, second_unit)
        second_record = run_registry.read_record(self.root / "ignored/validate/runs" / f"{second_unit}.json")
        self.assertTrue(start_unit.cell_ledger_published(second_record))
        self.assertEqual(second_target, second_record["ledger_mirror"]["scorecard"]["hermit_commit"])
        self.assertEqual(SHA, first_record["ledger_mirror"]["scorecard"]["hermit_commit"])
        self.assertFalse(second_handoff.exists())
        self.assertFalse(first_handoff.exists())


    def test_materialize_target_launcher_refusal_cleans_exact_checkout_not_source(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.systemd_launch_rc = 1
        self.fake.systemd_launch_stderr = "systemd-run: exact launcher refusal\n"
        before = self.worktree_bytes(self.checkout)

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn(self.fake.systemd_launch_stderr, error)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertEqual(self.root / "worktrees/validate", exact_checkout.parent)
        self.assert_main_checkout_and_cargo_removed(exact_checkout)
        self.assertEqual("d" * 40, self.fake.source_head)
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertIn(field, record)
        self.assertEqual([Path(record["runtime_root"])], self.removed_temporary_paths())

    def test_materialize_target_rc75_archives_receipt_before_cleanup(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.actual_exit = start_unit.EXIT_COULD_NOT_DETERMINE
        self.fake.final_validate_status = "COULD_NOT_RUN"
        self.fake.executed_nodes = 0
        self.fake.executed_tests = None
        self.fake.passed_tests = None
        self.fake.ledger_rows = []
        self.fake.inner_log_lines = [
            "validate-run: REFUSED: exact launcher causal stderr"
        ]
        self.plant_orphan_receipt()
        before = self.worktree_bytes(self.checkout)

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("exact launcher causal stderr", error)
        self.assertIn("inner validate exited 75", error)
        self.assertIn("ORPHANED-RECEIPT", error)
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-test/.hermit-validate-ledger.jsonl"
        )
        self.assertTrue(archived.is_file())
        self.assert_main_checkout_and_cargo_removed()
        self.assertEqual("d" * 40, self.fake.source_head)
        self.assertEqual(before, self.worktree_bytes(self.checkout))

    def test_materialize_target_cannot_be_combined_with_in_place(self) -> None:
        rc, _output, error = self.invoke(["--materialize-target", "--in-place"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("cannot use --in-place", error)
        self.assertEqual([], self.fake.commands)

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
        self.assertIn("--wait-lock", create)
        self.assertEqual(
            str(start_unit.WRKSLOTS_CREATE_LOCK_WAIT_SECONDS),
            create[create.index("--wait-lock") + 1],
        )
        disk_service = (
            Path(__file__).resolve().parents[3]
            / "scripts/systemd/hermit-disk-snapshot.service"
        ).read_text()
        disk_timeout = int(
            next(
                line.removeprefix("TimeoutStartSec=")
                for line in disk_service.splitlines()
                if line.startswith("TimeoutStartSec=")
            )
        )
        self.assertGreater(
            start_unit.WRKSLOTS_CREATE_LOCK_WAIT_SECONDS,
            disk_timeout,
            "validation admission must outwait the complete disk service bound",
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
        self.assertEqual("json", create[create.index("--format") + 1])
        records = list((self.root / "ignored/validate/runs").glob("*.json"))
        self.assertEqual(1, len(records))
        durable = start_unit.run_registry.read_record(records[0])
        self.assertEqual(self.fake.fresh.name, durable["wrkslots_slot"])
        self.assertEqual(
            self.fake.wrkslots_generation, durable["wrkslots_generation"]
        )

    def test_malformed_wrkslots_create_result_refuses_before_admission(self) -> None:
        self.fake.wrkslots_create_output = '{"slot":"wrong"}'

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("unexpected result shape", error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_wrkslots_create_result_for_another_path_refuses_before_admission(self) -> None:
        self.fake.wrkslots_create_path = "/tmp/not-ours"

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("does not identify the created checkout", error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

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
        templates = [
            Path(command[2])
            for command in self.fake.commands
            if command[:2] == ["mktemp", "-d"]
            and len(command) > 2
            and "validate-cargo-" in command[2]
        ]
        self.assertEqual(
            [self.root / "ignored/validate/cargo-homes/validate-cargo-XXXXXXXX"],
            templates,
            "private Cargo homes must not appear as unregistered validation slots",
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
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_unregistered_submodule_after_update_refuses_before_admission(self) -> None:
        """A populated gitlink Git calls `-` makes hermit's lint node NO_RESULT.

        `git submodule update --init` is what re-registers a submodule whose
        shared registration another worktree's `deinit` removed. If a gap
        survives it, launching would spend a full run to learn that a node
        cannot judge the tree. The launch must refuse, name the shared config,
        and remove the checkout it created.
        """
        self.fake.fresh_submodule_status = [f"-{SHA} agent-utils\n"]

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("agent-utils (not initialized)", error)
        self.assertIn("every linked worktree of this repository shares", error)
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            ),
            "nothing may be admitted from a tree with an unregistered submodule",
        )
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)
        fresh_git = ["git", "-C", str(self.fake.fresh)]
        update = self.fake.commands.index(
            fresh_git + ["submodule", "update", "--init", "--recursive"]
        )
        capture = self.fake.commands.index(
            fresh_git + ["config", "--local", "--null", "--get-regexp", r"^submodule\."]
        )
        status = self.fake.commands.index(fresh_git + ["submodule", "status"])
        self.assertLess(update, status, "the check must judge the tree AFTER the update")
        self.assertLess(update, capture, "capture the registration the update wrote")
        self.assertLess(capture, status)
        self.assertEqual(
            ["run"],
            self.fake.fresh_submodule_status_views,
            "the gate must judge the view the run will get, through its override",
        )

    def test_submodule_status_gaps_each_refuse_fresh_checkout(self) -> None:
        cases = {
            f"+{SHA} agent-utils (heads/main)\n": "agent-utils (checked out at a different commit than recorded)",
            f"U{SHA} agent-utils\n": "agent-utils (in a merge conflict)",
            f" {SHA} liteinst (v1)\n-{SHA} agent-utils\n": "agent-utils (not initialized)",
        }
        for status, expected in cases.items():
            with self.subTest(status=status):
                self.fake.fresh_submodule_status = [status]
                self.fake.fresh_submodule_status_calls = 0
                self.fake.removed.clear()
                with self.assertRaisesRegex(RuntimeError, re.escape(expected)) as caught:
                    start_unit.prepare_fresh_checkout(
                        self.checkout,
                        SHA,
                        state_root=self.root,
                        run=self.fake,
                        parent=self.root / "worktrees" / "validate",
                        repo="rrnewton/hermit",
                    )
                self.assertIn("registered checkout was removed", str(caught.exception))
                self.assertNotIn("liteinst", str(caught.exception))
                self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def fresh_unit_setenv(self) -> dict[str, str]:
        launch = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        pairs = [
            launch[index + 1].split("=", 1)
            for index, argument in enumerate(launch)
            if argument == "--setenv"
        ]
        return {name: value for name, value in pairs}

    def test_the_unit_runs_with_the_captured_submodule_registration(self) -> None:
        """Finding: a deinit AFTER creation must not reach the running checkout.

        The registration is captured into the checkout's own Git directory and
        the unit gets exactly one include override, scoped to that directory.
        test_submodule_registration.py shows with real Git that this keeps the
        run at ` ` through a peer's deinit and reaches no other repository.
        """
        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        git_dir = self.fake.fresh_git_dir().resolve()
        registration = git_dir / "validate-submodule-registration"
        setenv = self.fresh_unit_setenv()
        self.assertEqual("1", setenv.get("GIT_CONFIG_COUNT"), setenv)
        self.assertEqual(f"includeIf.gitdir:{git_dir}.path", setenv["GIT_CONFIG_KEY_0"])
        self.assertEqual(str(registration), setenv["GIT_CONFIG_VALUE_0"])
        written = subprocess.run(
            ["git", "config", "--file", str(registration), "--list"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        self.assertEqual(
            "submodule.agent-utils.url=https://github.com/rrnewton/agent-utils.git\n"
            "submodule.agent-utils.active=true\n",
            written,
            "only the variables deinit deletes are captured; update policy stays shared",
        )

    def test_a_shared_registration_removed_mid_run_is_named_with_restores(self) -> None:
        """Measured 2026-09-29: a deinit in ANOTHER worktree, mid-run.

        Full validate s17 created its checkout at 02:49:08Z with agent-utils
        registered. At 02:50:41Z an agent ran `git submodule deinit -f
        agent-utils` in its own scratch hermit worktree, which deletes the
        section from the config every worktree shares. check.lint_checks then
        exited 75. With the captured registration the run keeps its view, and
        the launcher names the shared loss for everyone else, with restore
        commands aimed at the checkout's OWN common Git directory -- here a
        managed repository distinct from the source checkout the launch named.
        """
        managed = self.root / "managed-hermit" / ".git"
        self.fake.fresh_common_dir = managed
        self.fake.fresh_shared_submodule_status = [f"-{SHA} agent-utils\n"]
        self.fake.fresh_submodule_config = [self.fake.fresh_submodule_config[0], ""]

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIn(
            "SUBMODULE-REGISTRATION-REMOVED-FROM-SHARED-CONFIG-DURING-RUN: "
            "agent-utils (not initialized)",
            error,
        )
        self.assertNotIn("SUBMODULE-REGISTRATION-LOST-DURING-RUN", error)
        self.assertIn(
            f"restore: git --git-dir={managed} config submodule.agent-utils.url "
            "https://github.com/rrnewton/agent-utils.git\n",
            error,
        )
        self.assertIn(
            f"restore: git --git-dir={managed} config submodule.agent-utils.active true\n",
            error,
        )
        self.assertNotIn(f"-C {self.checkout}", error)
        self.assertEqual(["run", "shared", "run"], self.fake.fresh_submodule_status_views)
        self.assertIn(
            str(self.fake.fresh), self.fake.removed, "the report must not block cleanup"
        )

    def test_a_registration_lost_from_the_run_view_is_named(self) -> None:
        self.fake.fresh_submodule_status = [
            f" {SHA} agent-utils (heads/main)\n",
            f"-{SHA} agent-utils\n",
        ]
        self.fake.fresh_shared_submodule_status = [f"-{SHA} agent-utils\n"]
        self.fake.fresh_submodule_config = [self.fake.fresh_submodule_config[0], ""]

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIn(
            "SUBMODULE-REGISTRATION-LOST-DURING-RUN: agent-utils (not initialized)", error
        )
        self.assertIn(
            f"restore: git --git-dir={self.checkout / '.git'} config "
            "submodule.agent-utils.active true",
            error,
        )
        self.assertIn(str(self.fake.fresh), self.fake.removed)

    def test_registered_submodules_emit_no_loss_report(self) -> None:
        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertNotIn("SUBMODULE-REGISTRATION", error)
        self.assertEqual(["run", "shared", "run"], self.fake.fresh_submodule_status_views)

    def test_a_submodule_the_run_moved_is_not_reported_as_lost(self) -> None:
        """A run may check a submodule out elsewhere; only `-` is a lost registration."""
        self.fake.fresh_submodule_status = [
            f" {SHA} agent-utils (heads/main)\n",
            f"+{SHA} agent-utils (heads/other)\n",
        ]
        self.fake.fresh_shared_submodule_status = [f"+{SHA} agent-utils (heads/other)\n"]

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertNotIn("SUBMODULE-REGISTRATION", error)
        self.assertEqual(["run", "shared", "run"], self.fake.fresh_submodule_status_views)

    def test_attach_reports_a_shared_loss_the_waiting_launcher_never_saw(self) -> None:
        """The WAIT-INTERRUPTED launcher returns before its own post-run report."""
        record, fresh = self.materialized_attach_record()
        git_dir = self.fake.fresh_git_dir().resolve()
        (git_dir / "validate-submodule-registration").write_text(
            start_unit.render_submodule_registration(
                [
                    ("submodule.agent-utils.url", "https://github.com/rrnewton/agent-utils.git"),
                    ("submodule.agent-utils.active", "true"),
                ]
            ),
            encoding="utf-8",
        )
        self.fake.fresh_shared_submodule_status = [f"-{SHA} agent-utils\n"]
        self.fake.fresh_submodule_config = [""]
        err = io.StringIO()

        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, err.getvalue())
        self.assertIn(
            "SUBMODULE-REGISTRATION-REMOVED-FROM-SHARED-CONFIG-DURING-RUN", err.getvalue()
        )
        self.assertIn(
            f"restore: git --git-dir={self.checkout / '.git'} config "
            "submodule.agent-utils.active true",
            err.getvalue(),
        )
        self.assertEqual(["shared", "run"], self.fake.fresh_submodule_status_views)
        self.assertEqual([str(fresh)], self.fake.removed)

    def test_new_dagrun_name_is_a_usable_fresh_checkout(self) -> None:
        self.fake.fresh_runner_name = "dagrun"

        fresh = start_unit.prepare_fresh_checkout(
            self.checkout,
            SHA,
            state_root=self.root,
            run=self.fake,
            parent=self.root / "worktrees" / "validate",
            repo="rrnewton/hermit",
        )

        self.assertEqual(self.fake.fresh, fresh[0])
        self.assertEqual(self.fake.wrkslots_generation, fresh[1].generation)

    def test_fresh_checkout_uses_the_exact_tooling_wrkslots(self) -> None:
        linked_tooling = self.root / "worktrees/slots/linked-parent"

        start_unit.prepare_fresh_checkout(
            self.checkout,
            SHA,
            state_root=self.root,
            run=self.fake,
            parent=self.root / "worktrees" / "validate",
            repo="rrnewton/hermit",
            tool_root=linked_tooling,
        )

        create = next(
            command
            for command in self.fake.commands
            if wrkslots_action(command, "create")
        )
        self.assertEqual(str(linked_tooling / "ci-hub/bin/wrkslots"), create[0])
        self.assertEqual(
            str(self.root),
            create[create.index("--project-root") + 1],
            "exact tooling must still mutate only the canonical project registry",
        )

    def test_every_wrkslots_action_has_one_global_local_machine_option(self) -> None:
        tool_root = self.root / "worktrees/slots/linked-parent"
        for action in WRKSLOTS_ACTIONS:
            with self.subTest(action=action):
                command = start_unit.wrkslots_command(
                    tool_root,
                    self.root,
                    action,
                    "argument",
                )
                self.assertEqual(
                    [
                        str(tool_root / "ci-hub/bin/wrkslots"),
                        "--project-root",
                        str(self.root),
                        "--machine",
                        start_unit.WRKSLOTS_MACHINE,
                        "--allow-existing-unregistered-worktrees",
                        action,
                        "argument",
                    ],
                    command,
                )
                assert_local_wrkslots_machine(command)

    def test_executable_wrkslots_path_is_centralized_in_command_builder(self) -> None:
        import ast

        tree = ast.parse(Path(start_unit.__file__).read_text())
        literal_owners = {
            function.name
            for function in ast.walk(tree)
            if isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef))
            and any(
                isinstance(node, ast.Constant)
                and node.value == "ci-hub/bin/wrkslots"
                for node in ast.walk(function)
            )
        }
        self.assertEqual({"wrkslots_command"}, literal_owners)

    def test_crate_without_runner_executable_is_not_usable(self) -> None:
        self.fake.fresh_runner_executable = False

        with self.assertRaisesRegex(RuntimeError, "common/bin/dagrun"):
            start_unit.prepare_fresh_checkout(
                self.checkout,
                SHA,
                state_root=self.root,
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
        ledger_suffix = [
            str(self.root / "ci-hub/ledger/validate_rows.py"),
            "rows",
            "--preserve-admission",
        ]
        kinds = [
            (
                "status"
                if command[1:3] == ["validate-status", "--sha"]
                else "identity"
                if command[-3:] == ledger_suffix
                else "remove"
            )
            for command in self.fake.commands
            if command[1:3] == ["validate-status", "--sha"]
            or command[-3:] == ledger_suffix
            or wrkslots_action(command, "remove")
        ]
        self.assertEqual(["status", "identity", "remove"], kinds)
        self.assert_main_checkout_and_cargo_removed()

    @mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None)
    def test_post_run_uses_tool_root_readers_with_state_root_authority(
        self, _create_pane: mock.Mock
    ) -> None:
        tool_root = self.split_tool_root()
        tool_before = self.tree_bytes(tool_root)
        self.assertFalse(
            (self.checkout / "agent-utils/rs/dagrun/Cargo.toml").exists()
        )

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
                    "validate-split-root",
                    "--",
                    "full",
                ],
                run=self.fake,
                environment=self.environment,
                root=tool_root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(0, rc, err.getvalue())
        self.assert_canonical_readers_use_split_roots(tool_root)
        publication = next(
            (index, command)
            for index, command in enumerate(self.fake.commands)
            if len(command) > 1 and command[1] == "publish-commit-status"
        )
        index, command = publication
        self.assertEqual(str(tool_root / "ci-hub/ci-hub"), command[0])
        self.assertEqual(self.root, self.fake.command_cwds[index][1])
        environment = self.fake.command_envs[index]
        self.assertIsNotNone(environment)
        assert environment is not None
        self.assertEqual(str(tool_root), environment["DEV_HERMIT_TOOL_ROOT"])
        self.assertEqual(str(self.root), environment["DEV_HERMIT_PARENT"])
        self.assertTrue(
            (self.root / "ignored/validate/runs/validate-split-root.json").is_file()
        )
        self.assertFalse((self.root / "ci-hub/ci-hub").exists())
        self.assertFalse((self.root / "ci-hub/ledger/validate_rows.py").exists())
        self.assertEqual(tool_before, self.tree_bytes(tool_root))

    def test_attach_recovers_unknown_run_via_tool_root_readers_and_state_ledger(
        self,
    ) -> None:
        tool_root = self.split_tool_root()
        tool_before = self.tree_bytes(tool_root)
        fresh = self.root / "worktrees/validate/validate-fresh-run1563"
        self.fake.fresh = fresh
        self.fake._materialize_fresh_checkout()
        record = self.root / "ignored/validate/runs/validate-run1563.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "state": "unknown",
                "result": "unknown",
                "unit": "validate-run1563.service",
                "target": SHA,
                "repo": "rrnewton/hermit",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "wrkslots_slot": fresh.name,
                "wrkslots_generation": 7,
                "e2e_result_root": str(
                    self.plant_per_cell_results("validate-run1563")
                ),
                "log": str(self.root / "run1563.log"),
                "started_at": "2099-01-01T00:00:00Z",
            },
        )
        self.assertFalse(
            (self.checkout / "agent-utils/rs/dagrun/Cargo.toml").exists()
        )

        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                [
                    "--state-root",
                    str(self.root),
                    "--attach",
                    "validate-run1563",
                ],
                run=self.fake,
                environment=self.environment,
                root=tool_root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(0, rc, err.getvalue())
        self.assertIn("verdict=VALIDATED exit=0", out.getvalue())
        self.assert_canonical_readers_use_split_roots(tool_root)
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )
        self.assertTrue(
            any(
                command[0] == str(tool_root / "ci-hub/bin/wrkslots")
                and "remove" in command
                for command in self.fake.commands
            )
        )
        self.assertFalse(fresh.exists())
        self.assertEqual(tool_before, self.tree_bytes(tool_root))

    def test_cleanup_uses_wrkslots_even_when_source_is_missing(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-source-gone"
        fresh.mkdir(parents=True)
        (fresh / ".git").write_text("gitdir: shared/worktrees/source-gone\n")
        self.fake.fresh = fresh
        self.fake.source_common_dir_available = False
        tool_root = self.root / "linked-tooling"

        removed = start_unit.remove_fresh_checkout(
            self.checkout,
            fresh,
            state_root=self.root,
            run=self.fake,
            tool_root=tool_root,
            wrkslots=start_unit.WrkslotsIdentity(
                fresh.name, self.fake.wrkslots_generation
            ),
        )

        self.assertTrue(removed)
        self.assertEqual([str(fresh)], self.fake.removed)
        self.assertTrue(
            any(
                command[0] == str(tool_root / "ci-hub/bin/wrkslots")
                and "remove" in command
                and "--validate-complete" in command
                and command[command.index("--project-root") + 1] == str(self.root)
                for command in self.fake.commands
            )
        )

    def test_direct_cleanup_does_not_confirm_dangling_symlink_as_removed(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-direct-dangling"
        fresh.mkdir(parents=True)
        (fresh / ".git").write_text("gitdir: shared/worktrees/direct-dangling\n")
        self.fake.fresh = fresh

        def leave_dangling(command: list[str], **kwargs: object):
            result = self.fake(command, **kwargs)
            if wrkslots_action(command, "remove"):
                fresh.symlink_to(fresh.parent / "missing-direct-target")
            return result

        removed = start_unit.remove_fresh_checkout(
            self.checkout,
            fresh,
            state_root=self.root,
            run=leave_dangling,
            tool_root=self.root,
            wrkslots=start_unit.WrkslotsIdentity(
                fresh.name, self.fake.wrkslots_generation
            ),
        )

        self.assertFalse(removed)
        self.assertTrue(fresh.is_symlink())

    def test_immediate_frozen_cleanup_uses_typed_wrkslots_recovery(self) -> None:
        source = self.root / "worktrees/slots/source-hermit"
        source.mkdir(parents=True)
        tool_root = self.root / "worktrees/slots/linked-tooling"
        fresh = (
            start_unit.frozen_checkout_parent(self.root)
            / "validate-fresh-immediate-frozen"
        )
        (fresh / ".git").mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-immediate-frozen.json"
        record.parent.mkdir(parents=True)
        record.write_text("{}\n")
        commands: list[list[str]] = []

        def run(command: list[str], **_kwargs: object):
            commands.append(command)
            shutil.rmtree(fresh)
            return completed(command, stdout="recovered frozen validation checkout\n")

        removed = start_unit.remove_fresh_checkout(
            source,
            fresh,
            state_root=self.root,
            run=run,
            tool_root=tool_root,
            completed_record=record,
        )

        self.assertTrue(removed)
        self.assertEqual(1, len(commands))
        command = commands[0]
        self.assertTrue(wrkslots_action(command, "recover"))
        self.assertEqual(str(tool_root / "ci-hub/bin/wrkslots"), command[0])
        self.assertEqual(str(self.root), command[command.index("--project-root") + 1])
        self.assertEqual(
            source.relative_to(self.root).as_posix(),
            command[command.index("--repository") + 1],
        )
        self.assertEqual(
            record.relative_to(self.root).as_posix(),
            command[command.index("--completed-record") + 1],
        )
        self.assertIn("--coordinator-authorized", command)
        self.assertEqual(
            str(fresh),
            command[command.index("--frozen-validate-checkout") + 1],
        )
        self.assertFalse(fresh.exists())

    def test_frozen_receipt_cleanup_routes_preserve_provider_refusal(self) -> None:
        source = self.root / "worktrees/slots/source-hermit"
        source.mkdir(parents=True)
        tool_root = self.root / "worktrees/slots/tooling"
        frozen_parent = start_unit.frozen_checkout_parent(self.root)
        self.addCleanup(shutil.rmtree, frozen_parent, True)
        for route in ("recorded", "immediate"):
            with self.subTest(route=route):
                fresh = frozen_parent / f"validate-fresh-{route}"
                (fresh / ".git").mkdir(parents=True)
                record = self.root / f"ignored/validate/runs/validate-{route}.json"
                fields = {
                    "schema_version": 1,
                    "state": "completed",
                    "result": "no-result",
                    "checkout": str(fresh),
                    "source_checkout": str(source),
                    "temporary_checkout": True,
                    "admission": start_unit.FROZEN_RESULT_ADMISSION,
                }
                start_unit.run_registry.write_record(record, fields)
                before = record.read_bytes()
                commands = []

                def run(command: list[str], **kwargs: object):
                    if wrkslots_action(command, "recover"):
                        commands.append(command)
                        return completed(command, rc=3, stderr="typed removal proof required")
                    return self.fake(command, **kwargs)

                error = io.StringIO()
                with contextlib.redirect_stderr(error):
                    if route == "recorded":
                        removed, archived, reason = start_unit.cleanup_recorded_checkout(
                            self.root, fields, record_path=record,
                            unit=f"validate-{route}", run=run, tool_root=tool_root,
                        )
                        self.assertIsNotNone(reason)
                    else:
                        result = start_unit.cleanup_fresh_checkout_preserving_receipts(
                            self.root, source, fresh, unit=f"validate-{route}",
                            run=run, tool_root=tool_root, wrkslots=None,
                            completed_record=record,
                        )
                        removed, archived = result.removed, result.archived
                self.assertFalse(removed)
                self.assertFalse(archived)
                self.assertIn("typed removal proof required", error.getvalue())
                after = record.read_bytes()
                if route == "recorded":
                    durable = json.loads(after)
                    self.assertEqual(
                        str(fresh), durable["checkout_removal_intent"]["path"]
                    )
                    self.assertNotIn("checkout_removed_at", durable)
                else:
                    self.assertEqual(before, after)
                self.assertTrue(fresh.is_dir())
                self.assertEqual(1, len(commands))
                command = commands[0]
                self.assertEqual(str(tool_root / "ci-hub/bin/wrkslots"), command[0])
                for flag, expected in (
                    ("--project-root", str(self.root)),
                    ("--repository", source.relative_to(self.root).as_posix()),
                    ("--completed-record", record.relative_to(self.root).as_posix()),
                    ("--frozen-validate-checkout", str(fresh)),
                ):
                    self.assertEqual(expected, command[command.index(flag) + 1])

    def test_frozen_cleanup_refuses_missing_record_and_wrong_roots(self) -> None:
        frozen_parent = start_unit.frozen_checkout_parent(self.root)
        self.addCleanup(shutil.rmtree, frozen_parent, True)
        fresh = frozen_parent / "validate-fresh-root-mismatch"
        (fresh / ".git").mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-root-mismatch.json"
        start_unit.run_registry.write_record(
            record, {"schema_version": 1, "checkout": str(fresh)}
        )
        before = record.read_bytes()
        cases = (
            (self.root, self.checkout, None, "requires its completed validation record"),
            (self.root / "other", self.checkout, record, "outside the managed or frozen"),
            (self.root, self.root.parent / "outside-source", record, "outside the project root"),
            (self.root, self.checkout, self.root.parent / "outside-record", "outside the project root"),
        )
        for root, source, completed_record, reason in cases:
            with self.subTest(reason=reason, root=root, source=source):
                error = io.StringIO()
                with contextlib.redirect_stderr(error):
                    removed = start_unit.remove_fresh_checkout(
                        source, fresh, state_root=root, run=self.fake,
                        tool_root=self.root, completed_record=completed_record,
                    )
                self.assertFalse(removed)
                self.assertIn(reason, error.getvalue())
                self.assertTrue(fresh.is_dir())
                self.assertEqual(before, record.read_bytes())
        self.assertEqual([], self.fake.commands)

    def test_frozen_cleanup_rejects_provider_success_with_retained_path(self) -> None:
        frozen_parent = start_unit.frozen_checkout_parent(self.root)
        self.addCleanup(shutil.rmtree, frozen_parent, True)
        fresh = frozen_parent / "validate-fresh-provider-kept-path"
        (fresh / ".git").mkdir(parents=True)
        record = self.root / "ignored/validate/runs/validate-kept-path.json"
        start_unit.run_registry.write_record(
            record, {"schema_version": 1, "checkout": str(fresh)}
        )
        error = io.StringIO()

        with contextlib.redirect_stderr(error):
            removed = start_unit.remove_fresh_checkout(
                self.checkout, fresh, state_root=self.root,
                run=lambda command, **kwargs: completed(command),
                completed_record=record,
            )

        self.assertFalse(removed)
        self.assertTrue(fresh.is_dir())
        self.assertIn("reported success but checkout still exists", error.getvalue())

    def test_frozen_pre_record_refusal_does_not_invent_cleanup_record(self) -> None:
        self.addCleanup(shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True)

        def run(command: list[str], **kwargs: object):
            result = self.fake(command, **kwargs)
            if command[:2] == ["git", "clone"]:
                # A frozen clone owns a Git directory, unlike a linked worktree.
                metadata = self.fake.fresh / ".git"
                metadata.unlink()
                metadata.mkdir()
            return result

        with mock.patch.object(
            start_unit, "prepare_runtime_root", side_effect=RuntimeError("runtime preparation refused")
        ):
            rc, _output, error = self.invoke(["--frozen-validate", "--", "full"], run=run)

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("runtime preparation refused", error)
        self.assertIn("requires its completed validation record", error)
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertFalse((self.root / "ignored/validate/runs/validate-test.json").exists())
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))
        self.assertFalse(any(wrkslots_action(command, "recover") for command in self.fake.commands))

    @mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None)
    def test_frozen_inner_refusal_threads_actual_record_from_worker_slot(self, _pane: mock.Mock) -> None:
        self.write_validation_service_schema(5)
        source = self.root / "worktrees/slots/worker-hermit"
        shutil.copytree(self.checkout, source)
        self.fake.checkout = source
        tool_root = self.root / "worktrees/slots/tooling"
        (tool_root / "ci-hub/validate").mkdir(parents=True)
        tool_schema = tool_root / "hermit" / start_unit.service_result.SCHEMA_RELATIVE_PATH
        tool_schema.parent.mkdir(parents=True)
        shutil.copyfile(source / start_unit.service_result.SCHEMA_RELATIVE_PATH, tool_schema)
        self.addCleanup(shutil.rmtree, start_unit.frozen_checkout_parent(self.root), True)
        self.fake.actual_exit = 75
        self.fake.final_validate_status = "COULD_NOT_RUN"
        self.fake.executed_nodes = 0
        self.fake.executed_tests = self.fake.passed_tests = None
        self.fake.detail = ["fixture tool-root refusal"]
        self.fake.inner_log_lines = ["validate-run: REFUSED: fixture tool-root refusal"]
        self.fake.write_frozen_result = False
        commands = []

        def run(command: list[str], **kwargs: object):
            if wrkslots_action(command, "recover") and "--frozen-validate-checkout" in command:
                commands.append(command)
                return completed(command, rc=3, stderr="typed removal proof required")
            result = self.fake(command, **kwargs)
            if command[:2] == ["git", "clone"]:
                metadata = self.fake.fresh / ".git"
                metadata.unlink()
                metadata.mkdir()
            if command[0] == "systemd-run":
                environment = dict(
                    value.split("=", 1)
                    for flag, value in zip(command, command[1:])
                    if flag == "--setenv" and "=" in value
                )
                sidecar = Path(environment["VALIDATE_SERVICE_RESULT_PATH"])
                fields = json.loads(sidecar.read_text())
                fields["selection_mode"] = fields["scorecard_writeback"] = None
                sidecar.write_text(json.dumps(fields) + "\n")
            return result

        out, error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(error):
            rc = start_unit.main(
                ["--checkout", str(source), "--state-root", str(self.root),
                 "--agent", "hermit-test", "--target", SHA, "--unit", "validate-incomplete",
                 "--frozen-validate", "--", "full"],
                run=run, environment=self.environment, root=tool_root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(75, rc, error.getvalue())
        self.assertIn("fixture tool-root refusal", error.getvalue())
        self.assertIn("typed removal proof required", error.getvalue())
        self.assertNotIn("RESULT-RECORDED", out.getvalue())
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertEqual(1, len(commands))
        command = commands[0]
        record_path = self.root / "ignored/validate/runs/validate-incomplete.json"
        for flag, expected in (
            ("--project-root", str(self.root)),
            ("--repository", source.relative_to(self.root).as_posix()),
            ("--completed-record", record_path.relative_to(self.root).as_posix()),
        ):
            self.assertEqual(expected, command[command.index(flag) + 1])
        self.assertEqual(str(tool_root / "ci-hub/bin/wrkslots"), command[0])
        record = start_unit.run_registry.read_record(record_path)
        self.assertEqual("completed", record["state"])
        self.assertEqual("no-result", record["result"])
        self.assertFalse(record["qualifying_receipt"])
        self.assertNotIn("checkout_removed_at", record)

    def test_immediate_cleanup_does_not_rmtree_managed_git_directory(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-corrupt-git"
        (fresh / ".git").mkdir(parents=True)

        def run(command: list[str], **_kwargs: object):
            self.fake.commands.append(command)
            return completed(command, rc=2, stderr="registry shape refused")

        removed = start_unit.remove_fresh_checkout(
            self.checkout,
            fresh,
            state_root=self.root,
            run=run,
            tool_root=self.root,
            wrkslots=start_unit.WrkslotsIdentity(fresh.name, 7),
        )

        self.assertFalse(removed)
        self.assertTrue(fresh.exists())
        self.assertTrue(any(wrkslots_action(command, "remove") for command in self.fake.commands))

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
        self.assert_main_checkout_and_cargo_removed()

    def test_main_success_couples_durable_checkout_cargo_and_runtime_cleanup(
        self,
    ) -> None:
        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        record_path = run_registry.record_path(self.root, "validate-test")
        record = run_registry.read_record(record_path)
        checkout = Path(record["checkout"])
        cargo_home = Path(record["cargo_home"])
        runtime_root = Path(record["runtime_root"])
        for path in (checkout, cargo_home, runtime_root):
            self.assertFalse(path.exists() or path.is_symlink(), path)
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertIn(field, record)
        self.assert_main_checkout_and_cargo_removed(checkout)
        self.assertEqual([runtime_root], self.removed_temporary_paths())
        checkout_provider = next(
            index
            for index, command in enumerate(self.fake.commands)
            if wrkslots_action(command, "remove")
        )
        cargo_provider = next(
            index
            for index, command in enumerate(self.fake.commands)
            if "--ownerless-validate-cargo-home" in command
        )
        runtime_provider = next(
            index
            for index, command in enumerate(self.fake.commands)
            if command[:2] == ["rm", "-rf"] and Path(command[-1]) == runtime_root
        )
        self.assertLess(checkout_provider, cargo_provider)
        self.assertLess(cargo_provider, runtime_provider)

    def test_production_fence_allows_same_launch_full_cleanup(self) -> None:
        with mock.patch.object(
            start_unit,
            "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
            PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertIn(field, record)
        for field in ("checkout", "cargo_home", "runtime_root"):
            path = Path(record[field])
            self.assertFalse(path.exists() or path.is_symlink(), path)

    def test_same_launch_cleanup_waits_for_foreign_generation_holder(self) -> None:
        rc, _output, error, holder = self.invoke_with_foreign_cleanup_lock(
            "generation", holder_seconds=0.3, wait_seconds=1.0
        )
        self.assertEqual(0, holder.wait(timeout=2), holder.stderr.read())
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assert_record_storage_removed()

    def test_same_launch_cleanup_waits_for_foreign_population_holder(self) -> None:
        rc, _output, error, holder = self.invoke_with_foreign_cleanup_lock(
            "population", holder_seconds=0.3, wait_seconds=1.0
        )
        self.assertEqual(0, holder.wait(timeout=2), holder.stderr.read())
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assert_record_storage_removed()

    def test_same_launch_cleanup_retains_after_generation_wait_deadline(self) -> None:
        rc, _output, error, holder = self.invoke_with_foreign_cleanup_lock(
            "generation", holder_seconds=2.0, wait_seconds=0.05
        )
        self.assertIsNone(holder.poll())
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn(
            "same-launch cleanup timed out after 0.05s waiting for "
            "run-writer generation authority",
            error,
        )
        self.assert_record_storage_retained()

    def test_same_launch_cleanup_retains_after_population_wait_deadline(self) -> None:
        rc, _output, error, holder = self.invoke_with_foreign_cleanup_lock(
            "population", holder_seconds=2.0, wait_seconds=0.05
        )
        self.assertIsNone(holder.poll())
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn(
            "same-launch cleanup timed out after 0.05s waiting for "
            "run-population authority",
            error,
        )
        self.assert_record_storage_retained()

    def test_same_launch_cleanup_does_not_retry_its_own_generation_lock(self) -> None:
        with (
            start_unit.run_registry.run_writer_generation_lock(self.root),
            mock.patch.object(
                start_unit,
                "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
                PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
            ),
            mock.patch.object(start_unit.time, "sleep") as lock_sleep,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn(
            "this process already holds run-writer generation authority", error
        )
        lock_sleep.assert_not_called()
        self.assert_record_storage_retained()

    def test_same_launch_cleanup_shares_one_generation_and_population_deadline(
        self,
    ) -> None:
        real_finalize = start_unit.finalize_commit_status_publication
        real_wait = start_unit._wait_for_cleanup_lock
        generation_holder: subprocess.Popen[str] | None = None
        population_holder: subprocess.Popen[str] | None = None
        observed: list[tuple[str, float]] = []

        def hold_both_after_publication(*args: object, **kwargs: object):
            nonlocal generation_holder, population_holder
            updated = real_finalize(*args, **kwargs)
            population_holder = self.foreign_cleanup_lock_holder(
                "population", seconds=2.0
            )
            generation_holder = self.foreign_cleanup_lock_holder(
                "generation", seconds=0.15
            )
            return updated

        def observe_wait(
            deadline: float,
            *,
            role: str,
            error: run_registry.RecordLocked,
        ) -> None:
            observed.append((role, deadline))
            real_wait(deadline, role=role, error=error)

        with (
            mock.patch.object(
                start_unit,
                "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
                PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
            ),
            mock.patch.object(
                start_unit, "SAME_LAUNCH_CLEANUP_LOCK_WAIT_SECONDS", 0.3
            ),
            mock.patch.object(
                start_unit, "SAME_LAUNCH_CLEANUP_LOCK_POLL_SECONDS", 0.005
            ),
            mock.patch.object(
                start_unit,
                "finalize_commit_status_publication",
                side_effect=hold_both_after_publication,
            ),
            mock.patch.object(
                start_unit, "_wait_for_cleanup_lock", side_effect=observe_wait
            ),
        ):
            rc, _output, error = self.invoke(["--", "full"])

        assert generation_holder is not None
        assert population_holder is not None
        self.assertEqual(0, generation_holder.wait(timeout=2))
        self.assertIsNone(population_holder.poll())
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("waiting for run-population authority", error)
        self.assertIn("run-writer generation authority", {row[0] for row in observed})
        self.assertIn("run-population authority", {row[0] for row in observed})
        self.assertEqual(1, len({row[1] for row in observed}), observed)
        self.assert_record_storage_retained()

    def test_same_launch_cleanup_does_not_retry_a_body_lock_error(self) -> None:
        with (
            mock.patch.object(
                start_unit,
                "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
                PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
            ),
            mock.patch.object(start_unit.time, "sleep") as lock_sleep,
        ):
            with self.assertRaisesRegex(run_registry.RecordLocked, "body failure"):
                with start_unit.destructive_cleanup_authority(
                    self.root, same_launch=True
                ):
                    raise run_registry.RecordLocked("body failure") from BlockingIOError()

        lock_sleep.assert_not_called()

    def test_same_launch_population_does_not_retry_a_body_lock_error(self) -> None:
        with (
            mock.patch.object(
                start_unit,
                "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
                PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
            ),
            mock.patch.object(start_unit.time, "sleep") as lock_sleep,
        ):
            with start_unit.destructive_cleanup_authority(
                self.root, same_launch=True
            ) as cleanup_authority:
                with self.assertRaisesRegex(run_registry.RecordLocked, "body failure"):
                    with start_unit.cleanup_population_authority(
                        self.root, cleanup_authority
                    ):
                        raise run_registry.RecordLocked(
                            "body failure"
                        ) from BlockingIOError()

        lock_sleep.assert_not_called()


    def test_same_launch_runtime_cleanup_retains_a_second_record_owner(self) -> None:
        real_finalize = start_unit.finalize_commit_status_publication
        second = run_registry.record_path(self.root, "validate-runtime-second-owner")

        def add_runtime_owner(*args: object, **kwargs: object):
            updated = real_finalize(*args, **kwargs)
            record_path = args[2]
            assert isinstance(record_path, Path)
            durable = run_registry.read_record(record_path)
            second.write_text(
                json.dumps(
                    {
                        "schema_version": run_registry.SCHEMA_VERSION,
                        "runtime_root": durable["runtime_root"],
                    }
                )
                + "\n"
            )
            return updated

        with mock.patch.object(
            start_unit,
            "finalize_commit_status_publication",
            side_effect=add_runtime_owner,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("runtime root is not owned exclusively", error)
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertIn("checkout_removed_at", record)
        self.assertIn("cargo_home_removed_at", record)
        self.assertNotIn("runtime_root_removed_at", record)
        self.assertTrue(Path(record["runtime_root"]).is_dir())

    def test_production_fence_same_launch_exemption_is_start_identity_bound(
        self,
    ) -> None:
        real_finalize = start_unit.finalize_commit_status_publication

        def replace_start_identity(*args: object, **kwargs: object):
            updated = real_finalize(*args, **kwargs)
            record_path = args[2]
            assert isinstance(record_path, Path)
            run_registry.update_record(
                record_path, started_at="2026-01-01T00:00:00+00:00"
            )
            return updated

        with (
            mock.patch.object(
                start_unit,
                "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED",
                PRODUCTION_CLEANUP_ROLLOUT_ENABLED,
            ),
            mock.patch.object(
                start_unit,
                "finalize_commit_status_publication",
                side_effect=replace_start_identity,
            ),
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("start identity does not match this launch", error)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertTrue(self.private_cargo_home().is_dir())
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertTrue(Path(record["runtime_root"]).is_dir())
        self.assertEqual([], self.fake.removed)

    def test_main_refuses_mismatched_durable_cargo_target_before_any_cleanup(
        self,
    ) -> None:
        real_finalize = start_unit.finalize_commit_status_publication
        wrong_cargo = self.private_cargo_home().with_name(
            "validate-cargo-durable-mismatch"
        )
        wrong_cargo.mkdir(parents=True)

        def tamper_after_publication(*args: object, **kwargs: object):
            updated = real_finalize(*args, **kwargs)
            record_path = args[2]
            assert isinstance(record_path, Path)
            run_registry.update_record(record_path, cargo_home=str(wrong_cargo))
            return updated

        with mock.patch.object(
            start_unit,
            "finalize_commit_status_publication",
            side_effect=tamper_after_publication,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("durable run record cargo_home", error)
        self.assertIn("does not match this launch", error)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertTrue(self.private_cargo_home().is_dir())
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertTrue(Path(record["runtime_root"]).is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertFalse(
            any(
                field in record
                for field in (
                    "checkout_removed_at",
                    "cargo_home_removed_at",
                    "runtime_root_removed_at",
                )
            )
        )

    def test_main_refuses_mismatched_durable_checkout_before_any_cleanup(
        self,
    ) -> None:
        real_finalize = start_unit.finalize_commit_status_publication
        wrong_checkout = (
            self.root
            / "worktrees/validate/validate-fresh-durable-mismatch"
        )
        wrong_checkout.mkdir(parents=True)

        def tamper_after_publication(*args: object, **kwargs: object):
            updated = real_finalize(*args, **kwargs)
            record_path = args[2]
            assert isinstance(record_path, Path)
            run_registry.update_record(record_path, checkout=str(wrong_checkout))
            return updated

        with mock.patch.object(
            start_unit,
            "finalize_commit_status_publication",
            side_effect=tamper_after_publication,
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("durable run record checkout", error)
        self.assertIn("does not match this launch", error)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())
        self.assertTrue(self.private_cargo_home().is_dir())
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertTrue(Path(record["runtime_root"]).is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertFalse(
            any(
                field in record
                for field in (
                    "checkout_removed_at",
                    "cargo_home_removed_at",
                    "runtime_root_removed_at",
                )
            )
        )

    def test_main_fresh_cargo_census_retains_second_owner_between_legs(
        self,
    ) -> None:
        second = run_registry.record_path(self.root, "validate-second-owner")
        injected = False
        protocol_writer_rejected = False

        def add_owner_after_checkout(command: list[str], **kwargs: object):
            nonlocal injected, protocol_writer_rejected
            result = self.fake(command, **kwargs)
            if not injected and wrkslots_action(command, "remove"):
                injected = True
                try:
                    run_registry.write_record(
                        second,
                        {
                            "schema_version": run_registry.SCHEMA_VERSION,
                            "cargo_home": str(self.private_cargo_home()),
                        },
                    )
                except run_registry.RecordLocked:
                    protocol_writer_rejected = True
                else:
                    self.fail("a current writer entered during main cleanup")
                # Model an unsupported pre-protocol writer bypassing the lock.
                # The fresh in-transaction census is a second line of defence:
                # it must observe the owner and retain the shared target.
                second.write_text(
                    json.dumps(
                        {
                            "schema_version": run_registry.SCHEMA_VERSION,
                            "cargo_home": str(self.private_cargo_home()),
                        }
                    )
                    + "\n"
                )
            return result

        rc, _output, error = self.invoke(["--", "full"], run=add_owner_after_checkout)

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(injected)
        self.assertTrue(protocol_writer_rejected)
        self.assertIn("not owned exclusively", error)
        self.assertTrue(self.private_cargo_home().is_dir())
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertIn("checkout_removed_at", record)
        self.assertNotIn("cargo_home_removed_at", record)
        self.assertNotIn("runtime_root_removed_at", record)
        self.assertTrue(Path(record["runtime_root"]).is_dir())
        self.assertEqual([str(Path(record["checkout"]))], self.fake.removed)
        self.assertFalse(
            any(
                "--ownerless-validate-cargo-home" in command
                for command in self.fake.commands
            )
        )

    def test_main_cargo_provider_failure_retains_cargo_and_runtime(self) -> None:
        self.fake.ownerless_cargo_recovery_rc = 3

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("ownerless validation recovery refused", error)
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertFalse(Path(record["checkout"]).exists())
        self.assertTrue(self.private_cargo_home().is_dir())
        self.assertTrue(Path(record["runtime_root"]).is_dir())
        self.assertIn("checkout_removed_at", record)
        self.assertNotIn("cargo_home_removed_at", record)
        self.assertNotIn("runtime_root_removed_at", record)
        self.assertEqual([str(Path(record["checkout"]))], self.fake.removed)

    def test_main_cargo_success_with_dangling_path_is_not_removal(self) -> None:
        def leave_dangling_cargo(command: list[str], **kwargs: object):
            result = self.fake(command, **kwargs)
            if "--ownerless-validate-cargo-home" in command:
                self.private_cargo_home().symlink_to(
                    self.root / "missing-cargo-target",
                    target_is_directory=True,
                )
            return result

        rc, _output, error = self.invoke(["--", "full"], run=leave_dangling_cargo)

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertIn("Cargo home still exists", error)
        self.assertTrue(self.private_cargo_home().is_symlink())
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertIn("checkout_removed_at", record)
        self.assertNotIn("cargo_home_removed_at", record)
        self.assertNotIn("runtime_root_removed_at", record)
        self.assertTrue(Path(record["runtime_root"]).is_dir())

    def test_runtime_root_removal_failure_is_discovered_and_retried(self) -> None:
        runtime = self.recoverable_runtime_root()
        failed_once = False

        def fail_first_runtime_remove(command: list[str], **kwargs: object):
            nonlocal failed_once
            if (
                not failed_once
                and command[:2] == ["rm", "-rf"]
                and Path(command[-1]) == runtime
            ):
                failed_once = True
                return completed(command, rc=1, stderr="injected runtime rm failure")
            return self.fake(command, **kwargs)

        with mock.patch.object(
            start_unit, "prepare_runtime_root", return_value=runtime
        ):
            rc, _output, error = self.invoke(
                ["--", "full"], run=fail_first_runtime_remove
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(failed_once)
        self.assertIn("injected runtime rm failure", error)
        record_path = run_registry.record_path(self.root, "validate-test")
        record = run_registry.read_record(record_path)
        self.assertEqual(runtime, Path(record["runtime_root"]))
        self.assertTrue(runtime.is_dir())
        self.assertNotIn("runtime_root_removed_at", record)
        self.assertTrue(start_unit.recorded_cleanup_path_exists(self.root, record))

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(runtime)], report["removed_runtime_roots"], report)
        self.assertFalse(runtime.exists() or runtime.is_symlink())
        self.assertIn(
            "runtime_root_removed_at", run_registry.read_record(record_path)
        )

    def test_runtime_retry_stamps_checkout_absent_after_crash_before_stamp(
        self,
    ) -> None:
        runtime = self.recoverable_runtime_root()
        real_update = start_unit.update_authorized_cleanup_record
        failed_once = False

        def fail_first_checkout_stamp(
            authority: start_unit.CleanupRecordAuthority,
            population: run_registry.RunPopulationAuthority,
            **fields: object,
        ) -> dict[str, object]:
            nonlocal failed_once
            if not failed_once and "checkout_removed_at" in fields:
                failed_once = True
                raise OSError(errno.EIO, "injected crash before checkout stamp")
            return real_update(authority, population, **fields)

        with (
            mock.patch.object(
                start_unit, "prepare_runtime_root", return_value=runtime
            ),
            mock.patch.object(
                start_unit,
                "update_authorized_cleanup_record",
                side_effect=fail_first_checkout_stamp,
            ),
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(failed_once)
        record_path = run_registry.record_path(self.root, "validate-test")
        record = run_registry.read_record(record_path)
        self.assertFalse(Path(record["checkout"]).exists())
        self.assertTrue(Path(record["cargo_home"]).is_dir())
        self.assertTrue(runtime.is_dir())
        self.assertNotIn("checkout_removed_at", record)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(runtime)], report["removed_runtime_roots"], report)
        recovered = run_registry.read_record(record_path)
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertIn(field, recovered)

    def test_runtime_retry_stamps_cargo_absent_after_crash_before_stamp(
        self,
    ) -> None:
        runtime = self.recoverable_runtime_root()
        real_update = start_unit.update_authorized_cleanup_record
        failed_once = False

        def fail_first_cargo_stamp(
            authority: start_unit.CleanupRecordAuthority,
            population: run_registry.RunPopulationAuthority,
            **fields: object,
        ) -> dict[str, object]:
            nonlocal failed_once
            if not failed_once and "cargo_home_removed_at" in fields:
                failed_once = True
                raise OSError(errno.EIO, "injected crash before Cargo-home stamp")
            return real_update(authority, population, **fields)

        with (
            mock.patch.object(
                start_unit, "prepare_runtime_root", return_value=runtime
            ),
            mock.patch.object(
                start_unit,
                "update_authorized_cleanup_record",
                side_effect=fail_first_cargo_stamp,
            ),
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(failed_once)
        record_path = run_registry.record_path(self.root, "validate-test")
        record = run_registry.read_record(record_path)
        self.assertFalse(Path(record["checkout"]).exists())
        self.assertFalse(Path(record["cargo_home"]).exists())
        self.assertTrue(runtime.is_dir())
        self.assertIn("checkout_removed_at", record)
        self.assertNotIn("cargo_home_removed_at", record)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(runtime)], report["removed_runtime_roots"], report)
        recovered = run_registry.read_record(record_path)
        for field in (
            "checkout_removed_at",
            "cargo_home_removed_at",
            "runtime_root_removed_at",
        ):
            self.assertIn(field, recovered)

    def test_in_place_runtime_root_removal_failure_is_retried(self) -> None:
        runtime = self.recoverable_runtime_root()
        failed_once = False

        def fail_first_runtime_remove(command: list[str], **kwargs: object):
            nonlocal failed_once
            if (
                not failed_once
                and command[:2] == ["rm", "-rf"]
                and Path(command[-1]) == runtime
            ):
                failed_once = True
                return completed(command, rc=1, stderr="injected runtime rm failure")
            return self.fake(command, **kwargs)

        with mock.patch.object(
            start_unit, "prepare_runtime_root", return_value=runtime
        ):
            rc, _output, error = self.invoke(
                ["--in-place", "--", "full"], run=fail_first_runtime_remove
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(failed_once)
        record_path = run_registry.record_path(self.root, "validate-test")
        record = run_registry.read_record(record_path)
        self.assertIs(record["temporary_checkout"], False)
        self.assertNotIn("checkout_removed_at", record)
        self.assertIn("cargo_home_removed_at", record)
        self.assertTrue(runtime.is_dir())

        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertEqual([str(runtime)], report["removed_runtime_roots"], report)
        self.assertFalse(runtime.exists() or runtime.is_symlink())
        self.assertIn(
            "runtime_root_removed_at", run_registry.read_record(record_path)
        )

    def test_runtime_root_recovery_refuses_outside_fixed_namespace(self) -> None:
        runtime = self.root / "vabcdefgh"
        runtime.mkdir()
        failed_once = False

        def fail_first_runtime_remove(command: list[str], **kwargs: object):
            nonlocal failed_once
            if (
                not failed_once
                and command[:2] == ["rm", "-rf"]
                and Path(command[-1]) == runtime
            ):
                failed_once = True
                return completed(command, rc=1, stderr="injected runtime rm failure")
            return self.fake(command, **kwargs)

        with mock.patch.object(
            start_unit, "prepare_runtime_root", return_value=runtime
        ):
            rc, _output, error = self.invoke(
                ["--", "full"], run=fail_first_runtime_remove
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(failed_once)
        record_path = run_registry.record_path(self.root, "validate-test")
        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertTrue(runtime.is_dir())
        self.assertEqual([], report["removed_runtime_roots"], report)
        self.assertTrue(
            any(
                row.get("checkout") == str(runtime)
                and "outside the fixed recovery namespace" in row["reason"]
                for row in report["retained"]
            ),
            report,
        )
        self.assertNotIn(
            "runtime_root_removed_at", run_registry.read_record(record_path)
        )

    def test_runtime_root_reappearing_after_stamp_is_retained_and_reported(self) -> None:
        runtime = self.recoverable_runtime_root()
        with mock.patch.object(
            start_unit, "prepare_runtime_root", return_value=runtime
        ):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        record_path = run_registry.record_path(self.root, "validate-test")
        record = run_registry.read_record(record_path)
        self.assertEqual(runtime, Path(record["runtime_root"]))
        self.assertFalse(runtime.exists() or runtime.is_symlink())
        self.assertIn("runtime_root_removed_at", record)

        runtime.mkdir()
        report = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )

        self.assertTrue(runtime.is_dir())
        self.assertEqual([], report["removed_runtime_roots"], report)
        self.assertTrue(
            any(
                row.get("checkout") == str(runtime)
                and "reappeared after its durable removal stamp" in row["reason"]
                for row in report["retained"]
            ),
            report,
        )

    def test_runtime_root_retry_refuses_a_nonterminal_unit(self) -> None:
        runtime = self.recoverable_runtime_root()
        failed_once = False

        def fail_first_runtime_remove(command: list[str], **kwargs: object):
            nonlocal failed_once
            if (
                not failed_once
                and command[:2] == ["rm", "-rf"]
                and Path(command[-1]) == runtime
            ):
                failed_once = True
                return completed(command, rc=1, stderr="injected runtime rm failure")
            return self.fake(command, **kwargs)

        with mock.patch.object(
            start_unit, "prepare_runtime_root", return_value=runtime
        ):
            rc, _output, error = self.invoke(
                ["--", "full"], run=fail_first_runtime_remove
            )
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(failed_once)
        record_path = run_registry.record_path(self.root, "validate-test")
        self.assertEqual(
            runtime, Path(run_registry.read_record(record_path)["runtime_root"])
        )

        def running(command: list[str], **kwargs: object):
            if command[:3] == ["systemctl", "--user", "show"]:
                return completed(
                    command,
                    stdout=(
                        "LoadState=loaded\nActiveState=active\nSubState=running\n"
                        "InvocationID=runtime-still-live\n"
                    ),
                )
            return self.fake(command, **kwargs)

        report = start_unit.sweep_completed_checkouts(
            self.root, run=running, tool_root=self.root
        )

        self.assertTrue(runtime.is_dir())
        self.assertEqual([], report["removed_runtime_roots"], report)
        self.assertTrue(
            any(
                row.get("checkout") == str(runtime)
                and "not provably terminal" in row["reason"]
                for row in report["retained"]
            ),
            report,
        )

    def test_runtime_root_retry_remains_behind_source_rollout_fence(self) -> None:
        runtime = self.recoverable_runtime_root()
        failed_once = False

        def fail_first_runtime_remove(command: list[str], **kwargs: object):
            nonlocal failed_once
            if (
                not failed_once
                and command[:2] == ["rm", "-rf"]
                and Path(command[-1]) == runtime
            ):
                failed_once = True
                return completed(command, rc=1, stderr="injected runtime rm failure")
            return self.fake(command, **kwargs)

        with mock.patch.object(
            start_unit, "prepare_runtime_root", return_value=runtime
        ):
            rc, _output, error = self.invoke(
                ["--", "full"], run=fail_first_runtime_remove
            )
        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        self.assertTrue(failed_once)
        record_path = run_registry.record_path(self.root, "validate-test")
        record = run_registry.read_record(record_path)
        self.assertEqual(runtime, Path(record["runtime_root"]))
        command_count = len(self.fake.commands)

        with mock.patch.object(
            start_unit, "DESTRUCTIVE_CLEANUP_ROLLOUT_ENABLED", False
        ):
            cleaned, reason = start_unit.cleanup_recorded_runtime_root(
                self.root,
                record,
                record_path=record_path,
                run=self.fake,
            )

        self.assertFalse(cleaned)
        self.assertIn("rollout-fenced", reason or "")
        self.assertTrue(runtime.is_dir())
        self.assertNotIn(
            "runtime_root_removed_at", run_registry.read_record(record_path)
        )
        self.assertFalse(
            any(
                command[:2] == ["rm", "-rf"]
                for command in self.fake.commands[command_count:]
            )
        )

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
        self.assertNotIn("NO-RECEIPT-PRODUCED", error)

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
        self.assert_main_checkout_and_cargo_removed()
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-test/.hermit-validate-ledger.jsonl"
        )
        self.assertTrue(archived.is_file(), "evidence must survive checkout removal")
        record = run_registry.read_record(
            run_registry.record_path(self.root, "validate-test")
        )
        self.assertEqual(
            [Path(record["runtime_root"])], self.removed_temporary_paths()
        )
        self.assertNotIn(self.private_cargo_home(), self.removed_temporary_paths())

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
        self.assert_main_checkout_and_cargo_removed()

    def test_no_receipt_cleanup_refusal_is_not_labeled_orphaned(self) -> None:
        self.fake.ledger_rows = []
        self.fake.wrkslots_remove_rc = 3
        self.fake.wrkslots_remove_stderr = "fixture no-receipt cleanup refusal"

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("NO-RECEIPT-PRODUCED", error)
        self.assertIn("fixture no-receipt cleanup refusal", error)
        self.assertNotIn("ORPHANED-RECEIPT", error)
        self.assertIsNotNone(self.fake.fresh)
        assert self.fake.fresh is not None
        self.assertTrue(self.fake.fresh.is_dir())

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
        self.assert_main_checkout_and_cargo_removed()
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
        self.assert_main_checkout_and_cargo_removed()

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
        self.assert_main_checkout_and_cargo_removed()

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
        self.assertIn("RECEIPT-CLASSIFICATION-UNAVAILABLE", error)
        self.assertNotIn("ORPHANED-RECEIPT", error)
        self.assertIn("scan failed", error)
        self.assertEqual([], self.fake.removed)
        self.assertTrue(
            (
                self.root
                / "ignored/validate/cargo-homes/validate-cargo-abcd1234"
            ).is_dir()
        )
        self.assertEqual([], self.removed_temporary_paths())

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
        self.assertNotIn("NO-RECEIPT-PRODUCED", error)
        # The property under test is IDENTITY BINDING -- a foreign row must not
        # satisfy the check. Retention is a separate question: this run left no
        # receipt in its own tree, so there is nothing to preserve and the tree
        # is reclaimed. Asserting retention here conflated the two.
        self.assert_main_checkout_and_cargo_removed()


class RunCommandCheckKwarg(unittest.TestCase):
    """`run_command` must HONOUR `check`, not forward it into `Popen`.

    ⚠️ THIS IS THE TEST THAT WAS MISSING WHEN THE FLEET WENT DOWN. Commit
    42d27103 moved `run_command` from `subprocess.run` to `subprocess.Popen` to
    wait on the child rather than on EOF -- a correct fix -- but kept passing
    `**kwargs` straight through. `check` is a `subprocess.run` parameter and
    `Popen` rejects it, so every one of the eight call sites that passes it
    raised `TypeError: Popen.__init__() got an unexpected keyword argument
    'check'`. `checked_output` is on the path of every launch, so
    `ci-hub validate-run` could not start a validation AT ALL, and the symptom
    was a bare TypeError with no diagnosis.

    Measured 2026-08-26: still reproducing on origin/main 15 hours after the fix
    was written, because the fix (dev-hermit#186) sat unmerged and nothing in the
    suite would have caught a reintroduction.

    Both directions, because a fix that only tests the happy path does not defend
    the load-bearing case: check=True in `create_fresh_checkout` must RAISE when
    `git worktree add` fails, rather than yield an empty tree that then validates
    fast and green.
    """

    def test_check_absent_returns_completed_process(self):
        result = start_unit.run_command(["true"])
        self.assertEqual(0, result.returncode)

    def test_check_false_does_not_reach_popen(self):
        """The exact crash: `check=False` must not be forwarded to Popen."""
        result = start_unit.run_command(["false"], check=False)
        self.assertEqual(1, result.returncode)

    def test_check_true_returns_on_success(self):
        result = start_unit.run_command(["true"], check=True)
        self.assertEqual(0, result.returncode)

    def test_check_true_raises_on_failure(self):
        """LOAD-BEARING: create_fresh_checkout relies on this raising."""
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            start_unit.run_command(["false"], check=True)
        self.assertEqual(1, caught.exception.returncode)

    def test_output_is_still_captured_with_check(self):
        """Honouring `check` must not cost the captured output."""
        result = start_unit.run_command(
            ["sh", "-c", "printf out; printf err >&2"], check=True
        )
        self.assertEqual("out", result.stdout)
        self.assertEqual("err", result.stderr)

    def test_checked_output_reaches_the_command(self):
        """`checked_output` passes check=False; it is on every launch path."""
        text = start_unit.checked_output(
            ["printf", "hello"], run=start_unit.run_command, purpose="probe"
        )
        self.assertEqual("hello", text)


class RunCommandTimeout(unittest.TestCase):
    """Exercise subprocess.run's timeout contract with real child processes."""

    def test_timeout_success_retains_both_streams(self):
        command = [sys.executable, "-B", "-c", "import os; os.write(1,b'out'); os.write(2,b'err')"]
        result = start_unit.run_command(command, check=True, timeout=5)
        self.assertEqual((0, "out", "err"), (result.returncode, result.stdout, result.stderr))
        self.assertEqual(command, result.args)

    def test_timeout_none_preserves_unbounded_wait(self):
        result = start_unit.run_command([sys.executable, "-B", "-c", "pass"], timeout=None)
        self.assertEqual(0, result.returncode)

    def test_timeout_nonzero_preserves_check_false_and_true(self):
        command = [sys.executable, "-B", "-c", "import os; os.write(1,b'out'); os.write(2,b'err'); raise SystemExit(7)"]
        result = start_unit.run_command(command, check=False, timeout=5)
        self.assertEqual((7, "out", "err"), (result.returncode, result.stdout, result.stderr))
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            start_unit.run_command(command, check=True, timeout=5)
        self.assertEqual(7, caught.exception.returncode)
        self.assertEqual(command, caught.exception.cmd)
        self.assertEqual("out", caught.exception.stdout)
        self.assertEqual("err", caught.exception.stderr)

    def test_timeout_kills_reaps_and_retains_raw_diagnostics(self):
        self._timeout_kills_reaps_and_retains_raw_diagnostics(check=False)

    def test_timeout_with_check_true_remains_timeout(self):
        self._timeout_kills_reaps_and_retains_raw_diagnostics(check=True)

    def _timeout_kills_reaps_and_retains_raw_diagnostics(self, *, check):
        command = [sys.executable, "-B", "-c", "import os,time; os.write(1,b'out\\xff'); os.write(2,b'err\\xfe'); time.sleep(30)"]
        real_popen = subprocess.Popen
        children = []

        def spawn(*args, **kwargs):
            child = real_popen(*args, **kwargs)
            children.append(child)
            return child

        try:
            with mock.patch.object(start_unit.subprocess, "Popen", side_effect=spawn):
                with self.assertRaises(subprocess.TimeoutExpired) as caught:
                    start_unit.run_command(command, check=check, timeout=1)
            self.assertEqual(command, caught.exception.cmd)
            self.assertEqual(1, caught.exception.timeout)
            # Like subprocess.run, TimeoutExpired retains bytes even
            # though completed calls return decoded text.
            self.assertEqual(b"out\xff", caught.exception.output)
            self.assertEqual(b"out\xff", caught.exception.stdout)
            self.assertEqual(b"err\xfe", caught.exception.stderr)
            self.assertEqual(1, len(children))
            self.assertEqual(-9, children[0].returncode)
            with self.assertRaises(ChildProcessError):
                os.waitpid(children[0].pid, os.WNOHANG)
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
                child.wait()

    def test_success_waits_for_child_not_inherited_output_descriptors(self):
        read_fd, write_fd = os.pipe()
        # The grandchild holds stdout/stderr open until the test releases it.
        # Waiting on EOF cannot satisfy this test; only direct-child exit can.
        code = (
            "import os,sys; child=os.fork(); "
            f"os.read({read_fd},1) if child == 0 else None; "
            "os._exit(0) if child == 0 else None; "
            "print('direct child exited', flush=True)"
        )
        try:
            result = start_unit.run_command(
                [sys.executable, "-B", "-c", code], timeout=1, pass_fds=(read_fd,)
            )
            self.assertEqual(0, result.returncode)
            self.assertEqual("direct child exited\n", result.stdout)
            self.assertEqual("", result.stderr)
        finally:
            os.close(write_fd)
            os.close(read_fd)


class CleanupRecordAdmissionTests(unittest.TestCase):
    """The cleanup rule must keep refusing a record it cannot decide.

    The companion fix publishes a refusal from validate-lock so this case stops
    being PRODUCED. The rule itself must not learn to infer the missing value:
    a record carrying no admission result does not say whether the validation
    payload ever started, and only a person can rule that both possible values
    would independently permit removal.
    """

    SHA = "a" * 40
    ROOT = Path("/tmp/cleanup-record-admission-tests")
    RECORD = ROOT / "ignored/validate/runs/validate-test.service.json"

    def handle(self, **updates):
        value = {
            "schema_version": run_registry.SCHEMA_VERSION,
            "kind": "validate",
            "state": "preparing",
            "unit": "validate-test.service",
            "target": self.SHA,
            "repo": "rrnewton/hermit",
            "checkout": "/tmp/checkout",
            "source_checkout": "/tmp/source",
            "temporary_checkout": True,
            "cargo_home": "/tmp/cargo",
            "log": "/tmp/validate.log",
            "agent": "type-audit",
            "pr": None,
            "started_at": "2026-08-28T07:00:00+00:00",
            "parent_checkout_head": self.SHA,
            "producer": run_registry.PRODUCER,
            "admission": "ci-hub validate-lock",
            "pane_role": "observer-only",
            "validate_lock_child_deadline_seconds": 3600,
            "e2e_result_root": "/tmp/e2e",
            "safe_ci_dag_runner_log_dir": "/tmp/dag",
            "hermit_run_timeout_seconds": 3300,
        }
        value.update(updates)
        return value

    def terminal(self, **updates):
        return self.handle(
            state="unknown",
            result="unknown",
            detail="final validate status is absent",
            **updates,
        )

    def test_missing_admission_result_is_refused_in_both_unit_states(self):
        """THE REGRESSION GUARD. Absent is undecidable, not permissive."""
        record = self.terminal()
        self.assertIsNone(record.get("admission_result"))
        for unit_absent in (True, False):
            with self.subTest(unit_absent=unit_absent):
                self.assertFalse(
                    start_unit.cleanup_record_allows_removal_attempt(
                        self.ROOT, self.RECORD, record, unit_absent=unit_absent
                    )
                )

    def test_explicit_null_admission_result_is_also_refused(self):
        """A field that is present and null reads as present to a bare lookup."""
        record = self.terminal(admission_result=None)
        self.assertIn("admission_result", record)
        self.assertFalse(
            start_unit.cleanup_record_allows_removal_attempt(
                self.ROOT, self.RECORD, record, unit_absent=True
            )
        )

    def test_refused_admission_without_durable_disposition_is_refused(self):
        record = self.terminal(
            admission_result={
                "state": "refused",
                "recorded_at": "2026-08-28T07:00:01+00:00",
                "reason": "validation-lock-refusal",
                "exit_code": 3,
            }
        )
        self.assertFalse(
            start_unit.cleanup_record_allows_removal_attempt(
                self.ROOT, self.RECORD, record, unit_absent=False
            )
        )

    def test_admitted_admission_needs_the_unit_to_be_absent(self):
        record = self.terminal(
            admission_result={
                "state": "admitted",
                "recorded_at": "2026-08-28T07:00:01+00:00",
                "run_number": 1799,
            }
        )
        self.assertTrue(
            start_unit.cleanup_record_allows_removal_attempt(
                self.ROOT, self.RECORD, record, unit_absent=True
            )
        )
        self.assertFalse(
            start_unit.cleanup_record_allows_removal_attempt(
                self.ROOT, self.RECORD, record, unit_absent=False
            )
        )

    def test_a_record_that_is_not_terminal_is_refused(self):
        record = self.handle(
            admission_result={
                "state": "admitted",
                "recorded_at": "2026-08-28T07:00:01+00:00",
                "run_number": 1799,
            }
        )
        self.assertFalse(
            start_unit.cleanup_record_allows_removal_attempt(
                self.ROOT, self.RECORD, record, unit_absent=True
            )
        )


class ReverieCountFinalizerTests(unittest.TestCase):
    def test_malformed_nested_sidecar_refuses_counts_and_preserves_failed_verdict(self) -> None:
        import hashlib
        from test_reverie_validate import count_transport_fixture
        for field in ("validate_lock_authority", "holder", "started_at", "finished_at"):
            for value in (None, [], 17):
                with self.subTest(field=field, value=value), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    path, record, binding = count_transport_fixture(root)
                    row = binding["canonical"]["row"]
                    if field == "holder":
                        row["validate_lock_authority"]["holder"] = value
                    else:
                        row[field] = value
                    binding["canonical_sha256"] = hashlib.sha256(
                        start_unit.reverie_node.count_json(binding["canonical"])
                    ).hexdigest()
                    run_registry.create_record(path, record)
                    start_unit.reverie_node.publish_count_sidecar(
                        start_unit.reverie_node.count_sidecar_path(path), binding
                    )
                    with mock.patch.object(start_unit.reverie_node, "read_finalization",
                                           side_effect=AssertionError("malformed transport must refuse before ledger query")):
                        with contextlib.redirect_stderr(io.StringIO()):
                            final = start_unit.finalize_reverie_counts(root, root, path, record=record)
                    for key in ("state", "result", "exit_code", "canonical_verdict", "wrapper_exit_code", "final_validate_status"):
                        self.assertEqual(record[key], final[key])
                    self.assertIsNone(final["executed_tests"])
                    self.assertIsNone(final["passed_tests"])
                    self.assertTrue(final["reverie_count_refusal"])
                    self.assertEqual(final, run_registry.read_record(path))

    def test_missing_sidecar_does_not_change_failed_result_or_wrapper_verdict(self) -> None:
        from test_reverie_validate import count_transport_fixture
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, record, _binding = count_transport_fixture(root)
            run_registry.create_record(path, record)
            with contextlib.redirect_stderr(io.StringIO()):
                final = start_unit.finalize_reverie_counts(root, root, path, record=record)
            for key in ("state", "result", "exit_code", "canonical_verdict", "wrapper_exit_code", "final_validate_status"):
                self.assertEqual(record[key], final[key])
            self.assertIsNone(final["executed_tests"])
            self.assertIsNone(final["passed_tests"])
            self.assertIn("No such file", final["reverie_count_refusal"])
            self.assertEqual(final, run_registry.read_record(path))

    def test_historical_handle_is_not_read_or_backfilled(self) -> None:
        record = {"kind": "reverie-validate", "passed_tests": None}
        with mock.patch.object(start_unit.reverie_node, "consume_count_handoff", side_effect=AssertionError("must not run")):
            self.assertIs(record, start_unit.finalize_reverie_counts(Path("/absent"), Path("/absent"), Path("/absent/handle.json"), record=record))

    def test_direct_and_attach_recovery_have_the_same_finalizer(self) -> None:
        import ast
        tree = ast.parse(Path(start_unit.__file__).read_text())
        callers = {node.name for node in tree.body if isinstance(node, ast.FunctionDef)
                   and any(isinstance(child, ast.Call) and isinstance(child.func, ast.Name)
                           and child.func.id == "finalize_reverie_counts" for child in ast.walk(node))}
        self.assertEqual({"main", "attach"}, callers)


if __name__ == "__main__":
    unittest.main()
