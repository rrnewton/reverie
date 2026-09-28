from __future__ import annotations

import contextlib
import fcntl
import hashlib
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import pathlib
import runpy
import unittest
from unittest import mock
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import start_unit  # noqa: E402
from test_immutable_tool_authority import AuthorityFixture, make_tree_read_only
import qualifying_receipt  # noqa: E402
from scripts import test_parent_main_write as parent_main_tests  # noqa: E402


SHA = "a" * 40
HERMIT_SHA = "b" * 40
AGENT_UTILS_SHA = "c" * 40


def completed(command: list[str], rc: int = 0, stdout: str = "", stderr: str = ""):
    return subprocess.CompletedProcess(command, rc, stdout, stderr)


def wrkslots_action(command: list[str], action: str) -> bool:
    return command[0].endswith("ci-hub/bin/wrkslots") and action in command



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
            slot = command[command.index("--slot") + 1]
            status_checkout = project_root / "worktrees/validate" / slot
            if self.scorecard_tamper_on_status is not None:
                tampered = status_checkout / self.scorecard_tamper_on_status
                tampered.write_text(tampered.read_text() + "tampered after writeback\n")
            relative = self.wrkslots_status_path or status_checkout.relative_to(
                project_root
            ).as_posix()
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
                                "path": relative,
                                "head": self.wrkslots_status_head or self.target,
                            }
                        ],
                    }
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
                common = self.checkout / ".git"
                common.mkdir(exist_ok=True)
                return completed(command, stdout=f"{common}\n")
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
            Path(output).write_text("\n".join(lines) + "\n")
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
            environment=environment or {"HOME": "/home/test", "PATH": "/usr/bin:/bin"},
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
        readers = [
            (command, cwd)
            for command, cwd in self.fake.command_cwds
            if command[1:3] == ["validate-status", "--sha"]
            or (len(command) >= 2 and command[-2].endswith("validate_rows.py"))
        ]
        self.assertEqual(2, len(readers))
        self.assertEqual(str(tool_root / "ci-hub/ci-hub"), readers[0][0][0])
        self.assertEqual(
            str(tool_root / "ci-hub/ledger/validate_rows.py"), readers[1][0][-2]
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
        self.assertEqual("codex/fixture-branch", record["branch"])
        self.assertEqual(3600, record["validate_lock_child_deadline_seconds"])
        self.assertEqual(3239, record["hermit_run_timeout_seconds"])

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

    def test_entry_cleanup_retains_completed_checkout_before_creating_next(self) -> None:
        stale, _record = self.managed_cleanup_record("entry-stale")

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(stale.exists())
        self.assertIn(
            "validate-run: ENTRY-CLEANUP state=ready removed=0 ", error
        )
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
        self.assertIn("retained-nonblocking=1", error)
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
        self.assertIn("retained-nonblocking=1", error)
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

    def test_entry_refuses_current_in_place_record_missing_temporary_checkout(
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("current run record has no temporary_checkout", error)
        self.assertFalse(
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

    def test_entry_refuses_unidentified_historical_in_place_record(self) -> None:
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("no recognized producer identity", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_refuses_malformed_in_place_temporary_checkout(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "malformed-in-place", temporary=None, current=True
        )
        before = record_path.read_bytes()

        with mock.patch.object(
            start_unit, "_carries_nosuid_or_nodev", return_value=False
        ):
            rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("temporary_checkout must be a boolean when present", error)
        self.assertFalse(
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
        self.assertIn("retained-nonblocking=1", error)
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

    def test_entry_still_refuses_legacy_in_place_record_without_journal(self) -> None:
        _checkout, _cargo_home, record_path = self.in_place_cleanup_record(
            "legacy-without-journal", include_temporary=False, current=False
        )
        before = record_path.read_bytes()
        rc, _output, error = self.invoke(["--dry-run"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("not under exactly one classified agent slot", error)
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
                observation = start_unit.terminal_cleanup_unit(record_path, record, run=run)
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
                    "reason": f"{expected_reason}; {storage_reason}",
                }], report["retained"])
                self.assertEqual(before, self.filesystem_snapshot(self.root))
                self.assertEqual(record, start_unit.run_registry.read_record(record_path))
                self.assertTrue(checkout.is_dir())
                self.assertTrue(cargo_home.is_dir())

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

    def test_entry_refuses_in_place_record_with_different_source_checkout(self) -> None:
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("checkout differs from its source checkout", error)
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

    def test_entry_refuses_malformed_current_record_inside_classified_slot(self) -> None:
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("current in-place run record is malformed", error)
        self.assertTrue(checkout.is_dir())
        self.assertTrue(cargo_home.is_dir())

    def test_entry_refuses_legacy_record_agent_different_from_classified_slot(self) -> None:
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("agent differs from its classified agent slot", error)
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
                        # Existing discovery skips rows whose only removable
                        # storage (the Cargo home) no longer exists.
                        retained_count = 0 if stage == "removed-cargo" else 4
                        self.assertIn(f"retained-nonblocking={retained_count}", error.getvalue())
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

    def test_entry_refuses_ambiguous_current_in_place_storage(self) -> None:
        checkout, cargo_home, record_path = self.in_place_cleanup_record(
            "ambiguous-in-place", temporary=False, current=True, state="unknown"
        )
        original = start_unit.run_registry.read_record(record_path)
        managed = self.root / "worktrees/validate/validate-fresh-not-in-place"
        absent_cargo = cargo_home.with_name("validate-cargo-already-removed")
        controls = (
            ({"checkout": "relative", "source_checkout": "relative"}, "absolute"),
            ({"checkout": str(managed), "source_checkout": str(managed)}, "temporary_checkout must be true"),
            ({"temporary_checkout": 0}, "temporary_checkout must be a boolean"),
            ({"cargo_home": None}, "malformed"),
            ({"cargo_home": str(self.root / "foreign-cargo")}, "Cargo home is outside"),
            ({"cargo_home": os.path.relpath(cargo_home)}, "absolute"),
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
                entry_expected = (
                    "unit state unavailable and run record is not terminal"
                    if refusal is not None else expected
                )
                if refusal is None:
                    reason, refusal = start_unit.classify_retained_current_in_place_record(
                        self.root, record
                    )
                    self.assertIsNone(reason)
                self.assertRegex(refusal or "", expected)
                # Preserve discovery's existing exclusion of rows with no
                # remaining cleanup path; the classifier above must still
                # refuse malformed or overlapping absent paths directly.
                if start_unit.recorded_cleanup_path_exists(self.root, record):
                    with self.assertRaisesRegex(RuntimeError, entry_expected):
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

    def test_entry_cleanup_refuses_after_one_read_only_bounded_backlog_batch(self) -> None:
        stale = [
            self.managed_cleanup_record(f"entry-backlog-{index:03d}")[0]
            for index in range(130)
        ]

        started = time.monotonic()
        rc, _output, error = self.invoke()
        elapsed = time.monotonic() - started

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertLess(elapsed, 1.0)
        self.assertEqual(0, sum(not path.exists() for path in stale))
        self.assertEqual(130, sum(path.exists() for path in stale))
        self.assertIn("removed 0 checkout(s)", error)
        self.assertIn("waiting on 122 deferred checkout(s)", error)
        self.assertIn(str(stale[8]), error)
        self.assertIn("ci-hub validate-run --sweep-completed", error)
        self.assertIn("the validate NEVER RAN", error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )

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
        self.assertIn("retained-nonblocking=1", error)
        self.assertFalse(
            any(
                wrkslots_action(command, "remove-validate-batch")
                for command in self.fake.commands
            )
        )
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_cleanup_blocks_running_frozen_singleton_after_admission_tamper(
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

        rc, _output, error = self.invoke(run=run)

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertTrue(frozen.is_dir())
        self.assertIn("unit-running", error)
        self.assertFalse(
            any(
                wrkslots_action(command, action)
                for command in self.fake.commands
                for action in ("recover", "recover-ownerless-validate-batch")
            )
        )
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
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
        self.assertIn("retained-nonblocking=1", error)
        entry_commands = self.fake.commands[command_count:]
        classify = next(
            command
            for command in entry_commands
            if wrkslots_action(command, "classify-ownerless-validate-batch")
        )
        self.assertNotIn("--coordinator-authorized", classify)
        self.assertNotIn("--coordinator-pid", classify)
        self.assertFalse(
            any(
                wrkslots_action(command, "recover-ownerless-validate-batch")
                for command in entry_commands
            )
        )
        self.assertTrue(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_cleanup_blocks_frozen_checkout_named_by_multiple_records(
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertTrue(frozen.is_dir())
        self.assertIn("must have exactly one run record; found 2", error)
        self.assertFalse(
            any(
                wrkslots_action(command, "recover-ownerless-validate-batch")
                for command in self.fake.commands
            )
        )
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_cleanup_does_not_infer_nonblocking_from_reason_text(self) -> None:
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertTrue(frozen.is_dir())
        self.assertIn("unit-running", error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

    def test_entry_cleanup_blocks_current_frozen_checkout_without_proof(self) -> None:
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

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertTrue(frozen.is_dir())
        self.assertIn("no typed removal proof", error)
        self.assertFalse(
            any(wrkslots_action(command, "create") for command in self.fake.commands)
        )

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
        self.assertIn("retained-nonblocking=1", error)
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
        publish_index = next(
            index
            for index, command in enumerate(self.fake.commands)
            if len(command) > 1 and command[1] == "publish-commit-status"
        )
        self.assertLess(receipt_index, scorecard_index)
        self.assertLess(scorecard_index, publish_index)
        self.assertLess(publish_index, cleanup_index)

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
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        publication = record["commit_status_publication"]
        self.assertEqual("published", publication["state"])
        self.assertEqual(SHA, publication["sha"])

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
        self.assertFalse(
            any(
                len(command) > 1 and command[1] == "publish-commit-status"
                for command in self.fake.commands
            )
        )
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(
            "scorecard write-back did not complete",
            record["commit_status_publication"]["reason"],
        )

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
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

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
        with contextlib.redirect_stdout(out):
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
        self.assertEqual(1, report["discovery"]["run_handle_candidates"])
        self.assertEqual(2, report["discovery"]["json_files_seen"])

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
        self.assertIn("has no run handle", reasons)
        self.assertEqual(0, report["discovery"]["framework_result_sidecars_skipped"])
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
        self.assertTrue(any("unsupported schema" in row["reason"] for row in report["retained"]))
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
        for suffix, admission in (
            (
                "admitted",
                {
                    "state": "admitted",
                    "recorded_at": "2026-09-04T12:00:00+00:00",
                    "run_number": 7,
                },
            ),
            (
                "refused",
                {
                    "state": "refused",
                    "recorded_at": "2026-09-04T12:00:00+00:00",
                    "reason": "stale-base",
                    "exit_code": 3,
                },
            ),
        ):
            with self.subTest(admission=admission["state"]):
                _fresh, record = self.current_unknown_cleanup_record(
                    suffix, admission
                )
                observation = start_unit.terminal_cleanup_unit(
                    self.root / f"validate-{suffix}.json",
                    record,
                    run=self.fake,
                )
                self.assertEqual(f"validate-{suffix}", observation.unit)
                self.assertEqual(
                    start_unit.CleanupUnitState.TERMINAL, observation.state
                )
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

    def test_cleanup_allows_refused_admission_when_unit_query_fails(self) -> None:
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
            self.root / "validate-refused-query-failed.json",
            record,
            run=failed_query,
        )

        self.assertEqual("validate-refused-query-failed", observation.unit)
        self.assertEqual(
            start_unit.CleanupUnitState.TERMINAL, observation.state
        )
        self.assertIsNone(observation.reason)
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
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

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
        self.assertEqual([], list(self.root.glob("v*")))

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
        self.assertIn(
            "compatibility scorecard: generated files changed", out.getvalue()
        )
        self.assertTrue(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))
        self.assertEqual(
            1,
            sum(
                len(command) > 1 and command[1] == "publish-commit-status"
                for command in self.fake.commands
            ),
        )
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_attach_terminal_materialized_scorecard_is_handed_off_then_cleaned(self) -> None:
        record, fresh = self.materialized_attach_record()
        self.fake.scorecard_update = True
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
        self.assertEqual([str(fresh)], self.fake.removed)
        handoff = start_unit.scorecard_handoff_path(self.root, "validate-test")
        self.assertTrue((handoff / "SCORECARD.md").read_text().endswith("updated\n"))
        self.assertEqual(
            str(handoff),
            start_unit.run_registry.read_record(record)["scorecard_handoff"],
        )
        self.assertIn(str(handoff), err.getvalue())
        writer_count = sum(
            command[0].endswith("scorecard.rs") for command in self.fake.commands
        )

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
            sum(command[0].endswith("scorecard.rs") for command in self.fake.commands),
        )
        self.assertIn("already-published output", second_err.getvalue())

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
        self.assertTrue(
            any("already-published verified" in row["reason"] for row in report["retained"]),
            report["retained"],
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

    def test_attach_after_inactive_sweep_copies_writeback_then_hands_off(self) -> None:
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
        handoff = start_unit.scorecard_handoff_path(self.root, "validate-test")
        self.assertEqual(str(handoff), durable["scorecard_handoff"])
        self.assertTrue(
            json.loads((handoff / "handoff.json").read_text())["writeback_completed"]
        )

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
        self.assertEqual([str(exact_checkout)], self.fake.removed)
        self.assertTrue(
            (
                start_unit.scorecard_handoff_path(self.root, "validate-test")
                / "handoff.json"
            ).is_file()
        )
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
        self.assertEqual([str(exact_checkout)], self.fake.removed)
        self.assertFalse(exact_checkout.exists())
        self.assertTrue(
            (
                start_unit.scorecard_handoff_path(self.root, "validate-test")
                / "handoff.json"
            ).is_file()
        )

    def test_materialized_scorecard_writeback_is_handed_off_before_cleanup(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        before = self.worktree_bytes(self.checkout)

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_PASSED, rc, error)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertEqual(before, self.worktree_bytes(self.checkout))
        self.assertEqual([str(exact_checkout)], self.fake.removed)
        self.assertFalse(exact_checkout.exists())
        handoff = start_unit.scorecard_handoff_path(self.root, "validate-test")
        self.assertTrue((handoff / "SCORECARD.md").read_text().endswith("updated\n"))
        self.assertTrue(
            (handoff / "ci/compat-envelope/cells.json")
            .read_text()
            .endswith("updated\n")
        )
        manifest = json.loads((handoff / "handoff.json").read_text())
        self.assertEqual(SHA, manifest["target"])
        self.assertEqual("validate-test", manifest["unit"])
        self.assertTrue(manifest["writeback_completed"])
        rows = {row["path"]: row for row in manifest["files"]}
        self.assertEqual(set(start_unit.SCORECARD_PATHS), set(rows))
        for relative in start_unit.SCORECARD_PATHS:
            payload = (handoff / relative).read_bytes()
            self.assertEqual(len(payload), rows[relative]["size"])
            self.assertEqual(
                hashlib.sha256(payload).hexdigest(), rows[relative]["sha256"]
            )
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(str(handoff), record["scorecard_handoff"])
        self.assertIn(str(handoff), error)
        sweep = start_unit.sweep_completed_checkouts(
            self.root, run=self.fake, tool_root=self.root
        )
        self.assertEqual([], sweep["removed"])
        self.assertTrue((handoff / "handoff.json").is_file())

    def test_materialized_scorecard_tamper_after_writeback_refuses_and_retains(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        self.fake.scorecard_tamper_on_status = "SCORECARD.md"

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertTrue(exact_checkout.is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertFalse(
            start_unit.scorecard_handoff_path(self.root, "validate-test").exists()
        )
        self.assertIn("differ from the writer-recorded identities", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(
            set(start_unit.SCORECARD_PATHS),
            {row["path"] for row in record["scorecard_writeback_files"]},
        )

    def test_materialized_scorecard_reused_generation_refuses_and_retains(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        self.fake.wrkslots_status_generation = self.fake.wrkslots_generation + 1

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
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

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
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

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertTrue(exact_checkout.is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertFalse(
            any(wrkslots_action(command, "status") for command in self.fake.commands)
        )
        self.assertIn("checkout HEAD is", error)

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

    def test_materialized_scorecard_handoff_failure_is_no_result_and_retains(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.scorecard_update = True
        handoff = start_unit.scorecard_handoff_path(self.root, "validate-test")
        handoff.parent.mkdir(parents=True)
        handoff.write_text("conflicting handoff\n")

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        exact_checkout = self.fake.fresh
        self.assertIsNotNone(exact_checkout)
        assert exact_checkout is not None
        self.assertTrue(exact_checkout.is_dir())
        self.assertEqual([], self.fake.removed)
        self.assertIn("SCORECARD HANDOFF NOT PUBLISHED", error)
        self.assertIn("retained for recovery", error)

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

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

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
                self.fake.fresh = None

    def test_materialize_target_refuses_dirty_fresh_checkout(self) -> None:
        self.fake.source_head = "d" * 40
        self.fake.fresh_dirty = " M scripts/validate.rs\n"

        rc, _output, error = self.invoke(["--materialize-target", "--", "full"])

        self.assertEqual(start_unit.EXIT_REFUSED, rc)
        self.assertIn("checkout is dirty", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_two_materialized_writebacks_need_no_source_cleanup_between_runs(
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
        first_bytes = self.tree_bytes(first_handoff)
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
        self.assertEqual(first_bytes, self.tree_bytes(first_handoff))
        second_handoff = start_unit.scorecard_handoff_path(self.root, second_unit)
        second_manifest = json.loads((second_handoff / "handoff.json").read_text())
        self.assertEqual(second_target, second_manifest["target"])
        self.assertEqual(second_unit, second_manifest["unit"])
        self.assertNotEqual(first_handoff, second_handoff)

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
        self.assertEqual([str(exact_checkout)], self.fake.removed)
        self.assertEqual("d" * 40, self.fake.source_head)
        self.assertEqual(before, self.worktree_bytes(self.checkout))

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
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)
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

    def test_new_dagrun_name_is_a_usable_fresh_checkout(self) -> None:
        self.fake.fresh_runner_name = "dagrun"

        fresh = start_unit.prepare_fresh_checkout(
            self.checkout,
            SHA,
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

    def test_immediate_frozen_cleanup_uses_typed_wrkslots_recovery(self) -> None:
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
            self.checkout,
            fresh,
            run=run,
            tool_root=self.root,
            completed_record=record,
        )

        self.assertTrue(removed)
        self.assertEqual(1, len(commands))
        command = commands[0]
        self.assertTrue(wrkslots_action(command, "recover"))
        self.assertIn("--coordinator-authorized", command)
        self.assertEqual(
            str(fresh),
            command[command.index("--frozen-validate-checkout") + 1],
        )
        self.assertFalse(fresh.exists())

    def test_immediate_cleanup_does_not_rmtree_managed_git_directory(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-corrupt-git"
        (fresh / ".git").mkdir(parents=True)

        def run(command: list[str], **_kwargs: object):
            self.fake.commands.append(command)
            return completed(command, rc=2, stderr="registry shape refused")

        removed = start_unit.remove_fresh_checkout(
            self.checkout,
            fresh,
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
            self.root / "ignored/validate/cargo-homes/validate-cargo-abcd1234",
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
            self.root / "ignored/validate/cargo-homes/validate-cargo-abcd1234",
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


if __name__ == "__main__":
    unittest.main()
