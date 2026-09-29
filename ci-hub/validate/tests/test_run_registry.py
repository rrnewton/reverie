#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import select
import stat
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import suppress
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[3]
# --import-mode=importlib does not put this directory on sys.path. Insert it
# here so the sibling test-module imports inside the tests below do not depend
# on another module having inserted it earlier in the same session.
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(ROOT / "ci-hub" / "validate"))

import run_registry  # noqa: E402


SHA = "a" * 40
SCORECARD_WRITEBACK_FILES = [
    {"path": "SCORECARD.md", "sha256": "b" * 64, "size": 123},
    {
        "path": "ci/compat-envelope/cells.json",
        "sha256": "c" * 64,
        "size": 456,
    },
]


def modern_publication() -> dict[str, object]:
    """The six-path file map emitted by the real schema-10 mirror producer."""
    objects = (
        "499bd720281f2b762b008a777c7dadd606e740243b65d1ffae288d7939457767",
        "60de04e985c3e7fa867636328b7b161e50c1d13024c1d1618beb2afc862018ab",
        "e4009d6798df2dbb634570edf51bc9e7e35f1aa35502228396c4e91a68e115ba",
    )
    carrier = "5cede5de0ab9c46b6be9b4933831428502ffb77c40d6868736319ecde7f351ca"
    repository = "https://github.com/rrnewton/hermit_test_ledger.git"
    return {
        "state": "published", "recorded_at": "2026-09-22T06:12:54Z",
        "repository": repository, "source_repository": repository,
        "commit": "e20023ee7f844ecc8f210a9d14c021ed91fadb2e",
        "source_commit": "2cfc26554ee9bfbaf1a8ca5f66648de43d635639",
        "series_commit": "2cfc26554ee9bfbaf1a8ca5f66648de43d635639",
        "source_tree": "091cffbefeb6c29fe3607538339b43b0e78e51e8",
        "rows": 88039, "raw_publication": {"published": 4},
        "scorecard": {
            "state": "published",
            "hermit_commit": "8ab53653770396e9490d7f7ea098648acee9a71f",
            "tool_hermit_commit": "8ab53653770396e9490d7f7ea098648acee9a71f",
            "files": {
                "scorecard/SCORECARD.md": "4fed3613279e61656b4a581afe5a6c71831dfdacb8590f9ea5109eb56ab31082",
                "scorecard/cells.json": "04cb4255f6bf0d2fcc3f649523bf1d4053c6fb140d87a9d1725ac6d7a01325c0",
                f"scorecard/finalized/runs/{carrier}.json":
                    "70a94f0f2dc95ec03343d82d8d1a1108b6a7eef520a28f604870776b083beacf",
                **{f"scorecard/finalized/objects/sha256/{digest}": digest for digest in objects},
            },
        },
    }


def validation_handle(**updates: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": run_registry.SCHEMA_VERSION,
        "kind": "validate",
        "state": "preparing",
        "unit": "validate-test.service",
        "target": SHA,
        "repo": "rrnewton/hermit",
        "checkout": "/tmp/checkout",
        "source_checkout": "/tmp/source",
        "temporary_checkout": True,
        "cargo_home": "/tmp/cargo",
        "log": "/tmp/validate.log",
        "agent": "type-audit",
        "pr": None,
        "started_at": "2026-08-28T07:00:00+00:00",
        "parent_checkout_head": SHA,
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


def bench_handle(**updates: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": run_registry.SCHEMA_VERSION,
        "kind": "bench",
        "state": "launching",
        "unit": "hermit-pressure-test.service",
        "target": SHA,
        "repo": "rrnewton/hermit",
        "checkout": "/tmp/pressure",
        "log": "/tmp/pressure.log",
        "agent": "tick-pressure-test",
        "started_at": "2026-08-28T07:00:00Z",
        "producer": run_registry.PRODUCER,
        "admission": "ci-hub validate-lock",
    }
    value.update(updates)
    return value


class RunRegistryContractTests(unittest.TestCase):
    def test_create_record_refuses_a_parent_replaced_after_install(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runs = root / "runs"
            displaced = root / "displaced-runs"
            runs.mkdir()
            path = runs / "validate-test.json"
            real_rename_noreplace = run_registry._rename_noreplace
            real_fsync = os.fsync
            syncs: list[tuple[str, int]] = []

            def rename_then_swap(
                parent_descriptor: int,
                source: str,
                destination: str,
                create_path: Path,
            ) -> None:
                real_rename_noreplace(
                    parent_descriptor, source, destination, create_path
                )
                runs.rename(displaced)
                runs.mkdir()

            def record_sync(descriptor: int) -> None:
                metadata = os.fstat(descriptor)
                syncs.append(
                    (
                        "directory" if stat.S_ISDIR(metadata.st_mode) else "file",
                        metadata.st_ino,
                    )
                )
                real_fsync(descriptor)

            with (
                mock.patch.object(
                    run_registry,
                    "_rename_noreplace",
                    side_effect=rename_then_swap,
                ),
                mock.patch.object(
                    run_registry.os, "fsync", side_effect=record_sync
                ),
            ):
                with self.assertRaisesRegex(
                    RuntimeError,
                    "parent or installed record changed before creation became durable",
                ):
                    run_registry.create_record(path, validation_handle())

            self.assertFalse(path.exists())
            self.assertEqual(
                validation_handle(),
                run_registry.read_record(displaced / path.name),
            )
            self.assertEqual(["file", "directory"], [kind for kind, _inode in syncs])
            self.assertEqual(displaced.stat().st_ino, syncs[-1][1])
            self.assertNotEqual(runs.stat().st_ino, syncs[-1][1])

    def test_create_record_does_not_clobber_after_preinstall_parent_swap(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runs = root / "runs"
            displaced = root / "displaced-runs"
            replacement = root / "replacement-runs"
            runs.mkdir()
            replacement.mkdir()
            path = runs / "validate-test.json"
            incumbent = validation_handle(agent="incumbent")
            (replacement / path.name).write_text(json.dumps(incumbent) + "\n")
            real_create = run_registry._create_unlocked

            def swap_then_create(
                create_path: Path,
                value: dict[str, object],
                *,
                parent_descriptor: int,
                parent_identity: tuple[int, ...],
            ) -> None:
                runs.rename(displaced)
                replacement.rename(runs)
                real_create(
                    create_path,
                    value,
                    parent_descriptor=parent_descriptor,
                    parent_identity=parent_identity,
                )

            with mock.patch.object(
                run_registry, "_create_unlocked", side_effect=swap_then_create
            ):
                with self.assertRaisesRegex(
                    RuntimeError,
                    "parent or installed record changed before creation became durable",
                ):
                    run_registry.create_record(path, validation_handle(agent="new"))

            self.assertEqual(incumbent, run_registry.read_record(path))
            self.assertEqual(
                validation_handle(agent="new"),
                run_registry.read_record(displaced / path.name),
            )

    def test_create_record_atomic_publication_preserves_racing_incumbent(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            runs = Path(temporary) / "runs"
            runs.mkdir()
            path = runs / "validate-test.json"
            incumbent = validation_handle(agent="incumbent")
            real_rename_noreplace = run_registry._rename_noreplace

            def create_incumbent_then_publish(
                parent_descriptor: int,
                source: str,
                destination: str,
                create_path: Path,
            ) -> None:
                path.write_text(json.dumps(incumbent) + "\n")
                real_rename_noreplace(
                    parent_descriptor, source, destination, create_path
                )

            with mock.patch.object(
                run_registry,
                "_rename_noreplace",
                side_effect=create_incumbent_then_publish,
            ):
                with self.assertRaisesRegex(RuntimeError, "already exists"):
                    run_registry.create_record(path, validation_handle(agent="new"))

            self.assertEqual(incumbent, run_registry.read_record(path))
            self.assertEqual([], list(runs.glob(".*.tmp")))

    def test_runtime_root_identity_is_optional_but_strict(self) -> None:
        legacy = run_registry.parse_current_record(validation_handle())
        self.assertNotIn("runtime_root_identity", legacy.raw)
        identity = {
            "device": 1,
            "inode": 2,
            "mode": 0o40700,
            "uid": 3,
            "gid": 4,
        }
        current = run_registry.parse_current_record(
            validation_handle(
                runtime_root="/tmp/vfixture",
                runtime_root_identity=identity,
            )
        )
        self.assertEqual(identity, current.raw["runtime_root_identity"])

        malformed = (
            {**identity, "inode": True},
            {**identity, "mode": 0o100600},
            {key: value for key, value in identity.items() if key != "gid"},
            {**identity, "extra": 5},
        )
        for value in malformed:
            with self.subTest(value=value), self.assertRaisesRegex(
                RuntimeError, "runtime_root_identity"
            ):
                run_registry.parse_current_record(
                    validation_handle(
                        runtime_root="/tmp/vfixture",
                        runtime_root_identity=value,
                    )
                )

    def test_recorded_github_route_is_optional_but_strictly_absolute(self) -> None:
        legacy = run_registry.parse_current_record(validation_handle())
        self.assertNotIn("github_executable", legacy.raw)
        current = run_registry.parse_current_record(
            validation_handle(github_executable="/retained/absolute/gh")
        )
        self.assertEqual("/retained/absolute/gh", current.raw["github_executable"])
        # Historical reads do not require an old executable to remain installed.
        for value in (None, True, 1, "", "gh"):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "github_executable"):
                run_registry.parse_current_record(validation_handle(github_executable=value))

    def test_validation_handle_has_typed_kind_state_and_projection(self) -> None:
        handle = run_registry.parse_current_record(validation_handle())
        self.assertEqual(1, handle.raw["schema_version"])
        self.assertEqual(run_registry.RunKind.VALIDATE, handle.kind)
        self.assertEqual(run_registry.RunState.PREPARING, handle.state)
        self.assertFalse(handle.lock_admissible)
        self.assertTrue(handle.counts_as_validation)
        self.assertIsNone(handle.admission_result)
        self.assertIsNone(run_registry.projection(handle.raw)["branch"])

    def test_run_handle_schema_is_an_exact_integer_at_every_boundary(self) -> None:
        missing = object()
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for label, schema_version in (
                ("missing", missing),
                ("boolean", True),
                ("floating", 1.0),
            ):
                with self.subTest(label=label):
                    value = validation_handle()
                    if schema_version is missing:
                        value.pop("schema_version")
                    else:
                        value["schema_version"] = schema_version

                    self.assertFalse(
                        run_registry.is_current_schema_version(
                            value.get("schema_version")
                        )
                    )
                    with self.assertRaisesRegex(RuntimeError, "schema"):
                        run_registry.parse_current_record(value)
                    with self.assertRaisesRegex(RuntimeError, "schema"):
                        run_registry.projection(value)

                    input_path = directory / f"{label}-input.json"
                    input_path.write_text(json.dumps(value))
                    with self.assertRaisesRegex(RuntimeError, "unsupported schema"):
                        run_registry.read_record(input_path)

                    output_path = directory / f"{label}-output.json"
                    with self.assertRaisesRegex(RuntimeError, "unsupported schema"):
                        run_registry.write_record(output_path, value)
                    self.assertFalse(output_path.exists())

    def test_validation_branch_provenance_is_optional_and_typed(self) -> None:
        current = validation_handle(branch="codex/validate-branch-provenance")
        handle = run_registry.parse_current_record(current)
        self.assertEqual(
            "codex/validate-branch-provenance",
            run_registry.projection(handle.raw)["branch"],
        )

        for branch in ("", 7):
            with self.subTest(branch=branch):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-branch"
                ):
                    run_registry.parse_current_record(
                        validation_handle(branch=branch)
                    )

    def test_materialized_target_provenance_is_optional_and_typed(self) -> None:
        historical = run_registry.parse_current_record(validation_handle())
        self.assertNotIn("materialized_target", historical.raw)
        current = run_registry.parse_current_record(
            validation_handle(
                materialized_target=True,
                wrkslots_slot="validate-fresh-exact",
                wrkslots_generation=7,
            )
        )
        self.assertIs(current.raw["materialized_target"], True)
        handed_off = run_registry.parse_current_record(
            validation_handle(
                materialized_target=True,
                wrkslots_slot="validate-fresh-exact",
                wrkslots_generation=7,
                scorecard_handoff="/state/ignored/validate/scorecard-writebacks/validate-test",
            )
        )
        self.assertEqual(
            "/state/ignored/validate/scorecard-writebacks/validate-test",
            handed_off.raw["scorecard_handoff"],
        )
        writer_bound = run_registry.parse_current_record(
            validation_handle(
                materialized_target=True,
                wrkslots_slot="validate-fresh-exact",
                wrkslots_generation=7,
                scorecard_writeback_files=SCORECARD_WRITEBACK_FILES,
            )
        )
        self.assertEqual(
            SCORECARD_WRITEBACK_FILES,
            writer_bound.raw["scorecard_writeback_files"],
        )

        for value in (None, 0, "true"):
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-materialized_target"
                ):
                    run_registry.parse_current_record(
                        validation_handle(materialized_target=value)
                    )

        for updates in (
            {"materialized_target": True},
            {
                "materialized_target": True,
                "temporary_checkout": False,
                "wrkslots_slot": "validate-fresh-exact",
                "wrkslots_generation": 7,
            },
            {
                "materialized_target": True,
                "source_checkout": "/tmp/checkout",
                "wrkslots_slot": "validate-fresh-exact",
                "wrkslots_generation": 7,
            },
        ):
            with self.subTest(updates=updates):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-materialized_target"
                ):
                    run_registry.parse_current_record(validation_handle(**updates))

        for updates in (
            {"scorecard_handoff": "/tmp/handoff"},
            {
                "repo": "rrnewton/reverie",
                "kind": "reverie-validate",
                "materialized_target": True,
                "wrkslots_slot": "validate-fresh-exact",
                "wrkslots_generation": 7,
                "scorecard_handoff": "/tmp/handoff",
            },
            {
                "materialized_target": True,
                "wrkslots_slot": "validate-fresh-exact",
                "wrkslots_generation": 7,
                "scorecard_handoff": "",
            },
        ):
            with self.subTest(scorecard_handoff=updates):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-scorecard_handoff"
                ):
                    run_registry.parse_current_record(validation_handle(**updates))

    def test_scorecard_writeback_files_are_a_closed_two_file_identity(self) -> None:
        base = {
            "materialized_target": True,
            "wrkslots_slot": "validate-fresh-exact",
            "wrkslots_generation": 7,
        }
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-scorecard_writeback_files"
        ):
            run_registry.parse_current_record(
                validation_handle(scorecard_writeback_files=SCORECARD_WRITEBACK_FILES)
            )
        cases = (
            [],
            SCORECARD_WRITEBACK_FILES[:1],
            [SCORECARD_WRITEBACK_FILES[0], SCORECARD_WRITEBACK_FILES[0]],
            [
                SCORECARD_WRITEBACK_FILES[0],
                {**SCORECARD_WRITEBACK_FILES[1], "sha256": "not-a-digest"},
            ],
            [
                SCORECARD_WRITEBACK_FILES[0],
                {**SCORECARD_WRITEBACK_FILES[1], "size": -1},
            ],
        )
        for value in cases:
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-scorecard_writeback_files"
                ):
                    run_registry.parse_current_record(
                        validation_handle(**base, scorecard_writeback_files=value)
                    )

    def test_validation_wrkslots_identity_is_paired_positive_and_optional(self) -> None:
        historical = run_registry.parse_current_record(validation_handle())
        self.assertNotIn("wrkslots_generation", historical.raw)

        current = run_registry.parse_current_record(
            validation_handle(
                wrkslots_slot="validate-fresh-exact",
                wrkslots_generation=7,
            )
        )
        self.assertEqual("validate-fresh-exact", current.raw["wrkslots_slot"])
        self.assertEqual(7, current.raw["wrkslots_generation"])

        for updates in (
            {"wrkslots_slot": "validate-fresh-exact"},
            {"wrkslots_generation": 7},
            {"wrkslots_slot": "validate-fresh-exact", "wrkslots_generation": 0},
            {"wrkslots_slot": "validate-fresh-exact", "wrkslots_generation": True},
        ):
            with self.subTest(updates=updates):
                with self.assertRaisesRegex(RuntimeError, "wrkslots"):
                    run_registry.parse_current_record(validation_handle(**updates))

        with self.assertRaisesRegex(RuntimeError, "unknown field.*wrkslots"):
            run_registry.parse_current_record(
                bench_handle(
                    wrkslots_slot="validate-fresh-exact",
                    wrkslots_generation=7,
                )
            )

    def test_termination_request_is_recorded_before_confirmation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "validate-test.json"
            run_registry.create_current_record(
                path,
                validation_handle(
                    state="running",
                    process_identity={
                        "pid": 99,
                        "start_ticks": 100,
                        "boot_id": "boot-one",
                    },
                ),
            )

            request = run_registry.record_termination_request(
                path,
                agent="terminating-agent",
                pid=4242,
                reason="superseded by an exact-head run",
                requested_at="2026-08-31T12:01:00+00:00",
            )
            before = run_registry.read_record(path)
            self.assertEqual([request], before["termination_requests"])
            self.assertNotIn("confirmed_at", request)

            confirmed = run_registry.confirm_termination_request(
                path,
                request,
                confirmed_at="2026-08-31T12:01:05+00:00",
            )
            self.assertEqual(
                "2026-08-31T12:01:05+00:00", confirmed["confirmed_at"]
            )
            projected = run_registry.projection(run_registry.read_record(path))
            self.assertEqual([confirmed], projected["termination_requests"])

    def test_normal_completion_has_no_termination_request(self) -> None:
        completed = validation_handle(
            state="completed",
            result="success",
            exit_code=0,
            detail="terminal detail retained for status readers",
            finished_at="2026-08-31T12:05:00+00:00",
            final_validate_status="PASSED",
        )

        handle = run_registry.parse_current_record(completed)

        self.assertEqual((), handle.termination_requests)
        projected = run_registry.projection(completed)
        self.assertEqual([], projected["termination_requests"])
        self.assertEqual("completed", projected["state"])
        self.assertEqual("success", projected["result"])
        self.assertEqual(
            "terminal detail retained for status readers", projected["detail"]
        )
        self.assertEqual(0, projected["exit_code"])
        self.assertEqual("2026-08-31T12:05:00+00:00", projected["finished_at"])

    def test_termination_refuses_before_process_identity_is_recorded(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "validate-test.json"
            run_registry.create_current_record(path, validation_handle(state="running"))

            with self.assertRaisesRegex(
                RuntimeError, "before the validation process identity is recorded"
            ):
                run_registry.record_termination_request(
                    path,
                    agent="terminating-agent",
                    pid=4242,
                    reason="superseded by an exact-head run",
                )

            self.assertNotIn("termination_requests", run_registry.read_record(path))

    def test_admission_result_is_closed_and_exact(self) -> None:
        handle = run_registry.parse_current_record(
            validation_handle(
                admission_result={
                    "state": "admitted",
                    "recorded_at": "2026-08-28T07:00:01+00:00",
                    "run_number": 701,
                }
            )
        )
        self.assertEqual(run_registry.AdmissionState.ADMITTED, handle.admission_result.state)
        self.assertEqual(701, handle.admission_result.run_number)

        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-admission_result.state: unsupported value 'waiting'",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    admission_result={
                        "state": "waiting",
                        "recorded_at": "2026-08-28T07:00:01+00:00",
                    }
                )
            )
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-admission_result.recorded_at: must include a UTC offset",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    admission_result={
                        "state": "admitted",
                        "recorded_at": "2026-08-28T07:00:01",
                    }
                )
            )

    def test_commit_status_publication_is_typed_but_optional(self) -> None:
        absent = run_registry.parse_current_record(validation_handle())
        self.assertNotIn("commit_status_publication", absent.raw)

        published = run_registry.parse_current_record(
            validation_handle(
                commit_status_publication={
                    "state": "published",
                    "recorded_at": "2026-08-31T10:00:00+00:00",
                    "repository": "rrnewton/hermit",
                    "sha": SHA,
                    "description": "Local full: 492 selected cells",
                    "receipt_commit": "b" * 40,
                    "receipt_path": "validation-receipts/rrnewton/hermit/receipt.json",
                }
            )
        )
        self.assertEqual(
            "published", published.raw["commit_status_publication"]["state"]
        )

        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-commit_status_publication: published must contain exactly",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    commit_status_publication={
                        "state": "published",
                        "recorded_at": "2026-08-31T10:00:00+00:00",
                        "repository": "rrnewton/hermit",
                        "sha": SHA,
                    }
                )
            )

    def test_ledger_mirror_result_is_typed_but_optional(self) -> None:
        absent = run_registry.parse_current_record(validation_handle())
        self.assertNotIn("ledger_mirror", absent.raw)

        published = run_registry.parse_current_record(
            validation_handle(
                ledger_mirror={
                    "state": "published",
                    "recorded_at": "2026-09-18T03:00:00+00:00",
                    "commit": "b" * 40,
                    "source_commit": "c" * 40,
                    "source_tree": "d" * 40,
                    "rows": 76445,
                }
            )
        )
        self.assertEqual("published", published.raw["ledger_mirror"]["state"])

        failed = run_registry.parse_current_record(
            validation_handle(
                ledger_mirror={
                    "state": "failed",
                    "recorded_at": "2026-09-18T03:00:00+00:00",
                    "detail": "remote push refused",
                }
            )
        )
        self.assertEqual("failed", failed.raw["ledger_mirror"]["state"])

        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-ledger_mirror: published must contain exactly",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    ledger_mirror={
                        "state": "published",
                        "recorded_at": "2026-09-18T03:00:00+00:00",
                    }
                )
            )

    def test_failed_publication_keeps_a_typed_quarantine_count(self) -> None:
        quarantine = {
            "schema": "series-quarantine-publication/v1", "published": 1,
            "already_published": 0, "commit": "b" * 40, "error": None,
            "pending_records": 0, "held_records": 0, "corrupt_files": 0,
            "set_aside": [],
            "run": {"run_id": "validate-x", "published": 1, "withheld": 0,
                    "unpublished": 0, "records": 1},
        }
        failed = {"state": "failed", "recorded_at": "2026-09-28T03:00:00+00:00",
                  "detail": "series batch refused",
                  "raw_publication": {"quarantine": quarantine}}
        accepted = run_registry.parse_current_record(validation_handle(ledger_mirror=failed))
        self.assertEqual(failed, accepted.raw["ledger_mirror"])
        not_attempted = {**quarantine, "published": 0, "commit": None,
                         "error": "not attempted: MirrorRefused: lock held",
                         "pending_records": 1,
                         "run": {**quarantine["run"], "published": None, "withheld": None,
                                 "unpublished": 1}}
        run_registry.parse_current_record(validation_handle(
            ledger_mirror={**failed, "raw_publication": {"quarantine": not_attempted}}))
        for label, raw in (
            ("series outcome smuggled in", {"quarantine": quarantine, "published": 3}),
            ("missing key", {"quarantine": {k: v for k, v in quarantine.items() if k != "run"}}),
            ("negative count", {"quarantine": {**quarantine, "published": -1}}),
            ("nullable only where declared", {"quarantine": {**quarantine, "published": None}}),
            ("bool is not a count", {"quarantine": {**quarantine, "pending_records": True}}),
            ("wrong schema", {"quarantine": {**quarantine, "schema": "v0"}}),
            ("short commit", {"quarantine": {**quarantine, "commit": "abc"}}),
            ("blank error", {"quarantine": {**quarantine, "error": " "}}),
            ("set_aside not names", {"quarantine": {**quarantine, "set_aside": [3]}}),
            ("run shape", {"quarantine": {**quarantine, "run": {"run_id": "x"}}}),
            ("run records nullable", {"quarantine": {**quarantine,
                                                     "run": {**quarantine["run"], "records": None}}}),
        ):
            with self.subTest(label), self.assertRaisesRegex(
                    RuntimeError, "ledger_mirror.raw_publication"):
                run_registry.parse_current_record(validation_handle(
                    ledger_mirror={**failed, "raw_publication": raw}))

    def test_direct_publication_binds_storage_and_preserves_partial_failure(self) -> None:
        repository = "https://github.com/rrnewton/hermit_test_ledger.git"
        publication = {
            "state": "published", "recorded_at": "2026-09-21T12:00:00Z",
            "repository": repository, "source_repository": repository,
            "commit": "b" * 40, "source_commit": "c" * 40,
            "source_tree": "d" * 40, "series_commit": "c" * 40, "rows": 2,
            "raw_publication": {"published": 2},
            "scorecard": {"state": "published", "hermit_commit": SHA,
                         "tool_hermit_commit": "e" * 40,
                         "files": {"scorecard/SCORECARD.md": "f" * 64,
                                   "scorecard/cells.json": "0" * 64}},
        }
        accepted = run_registry.parse_current_record(validation_handle(ledger_mirror=publication))
        self.assertEqual(publication, accepted.raw["ledger_mirror"])
        for field, bad in (("repository", "https://example.invalid/other"),
                           ("source_repository", "https://github.com/rrnewton/dev-hermit.git"),
                           ("series_commit", "a" * 40)):
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                run_registry.parse_current_record(validation_handle(ledger_mirror={**publication, field: bad}))
        for scorecard in ({"state": "published"},
                          {**publication["scorecard"], "hermit_commit": "unknown"},
                          {**publication["scorecard"], "files": {}},
                          {**publication["scorecard"], "tool_hermit_commit": "unknown"}):
            with self.subTest(scorecard=scorecard), self.assertRaises(RuntimeError):
                run_registry.parse_current_record(validation_handle(ledger_mirror={**publication, "scorecard": scorecard}))
        partial = {**publication, "state": "derived-failed", "detail": "renderer failed",
                   "scorecard": {"state": "failed", "error": "renderer failed"}}
        accepted = run_registry.parse_current_record(validation_handle(ledger_mirror=partial))
        self.assertEqual("c" * 40, accepted.raw["ledger_mirror"]["series_commit"])
        self.assertEqual("derived-failed", accepted.raw["ledger_mirror"]["state"])

    def test_direct_publication_records_a_proof_beside_a_failed_or_superseded_scorecard(self) -> None:
        publication = modern_publication()
        files = publication["scorecard"]["files"]
        proof_files = {path: digest for path, digest in files.items() if "/finalized/" in path}
        proof = {"state": "published", "commit": publication["commit"], "files": proof_files,
                 "hermit_commit": SHA, "tool_hermit_commit": "e" * 40}
        failed = {**publication, "state": "derived-failed", "detail": "renderer failed",
                  "scorecard": {"state": "failed", "error": "renderer failed"}, "portable_proof": proof}
        superseded = {**publication, "scorecard": {
            "state": "superseded", "hermit_commit": SHA, "current_hermit_commit": "b" * 40,
            "detail": "published scorecard head is newer"}}
        accepted_cases = (
            failed,
            {**failed, "portable_proof": {**proof, "state": "unchanged"}},
            {**failed, "portable_proof": {"state": "failed", "error": "census refused"}},
            superseded,
            {**superseded, "portable_proof": proof},
        )
        for candidate in accepted_cases:
            with self.subTest(accepted=candidate.get("portable_proof", candidate["scorecard"])):
                accepted = run_registry.parse_current_record(validation_handle(ledger_mirror=candidate))
                self.assertEqual(candidate, accepted.raw["ledger_mirror"])
        carrier = next(path for path in proof_files if "/runs/" in path)
        refused_cases = {
            "failed proof on a published record": {**superseded, "portable_proof": {"state": "failed", "error": "x"}},
            "failed proof without error": {**failed, "portable_proof": {"state": "failed", "error": ""}},
            "proof beside a regenerated scorecard": {**publication, "portable_proof": proof},
            "unknown proof state": {**failed, "portable_proof": {**proof, "state": "pending"}},
            "proof commit not forty-hex": {**failed, "portable_proof": {**proof, "commit": "unknown"}},
            "proof without carrier": {**failed, "portable_proof": {**proof, "files": {
                path: digest for path, digest in proof_files.items() if path != carrier}}},
            "proof without files": {**failed, "portable_proof": {**proof, "files": {}}},
            "proof with scorecard file": {**failed, "portable_proof": {**proof, "files": {
                **proof_files, "scorecard/SCORECARD.md": "f" * 64}}},
            "proof with extra field": {**failed, "portable_proof": {**proof, "extra": 1}},
            "superseded by itself": {**superseded, "scorecard": {
                **superseded["scorecard"], "current_hermit_commit": SHA}},
            "superseded without detail": {**superseded, "scorecard": {
                **superseded["scorecard"], "detail": " "}},
            "superseded head not forty-hex": {**superseded, "scorecard": {
                **superseded["scorecard"], "current_hermit_commit": "newer"}},
            "superseded with files": {**superseded, "scorecard": {
                **superseded["scorecard"], "files": files}},
            "superseded on a derived-failed record": {**failed, "scorecard": superseded["scorecard"]},
        }
        for label, candidate in refused_cases.items():
            with self.subTest(refused=label), self.assertRaises(RuntimeError):
                run_registry.parse_current_record(validation_handle(ledger_mirror=candidate))

    def test_direct_publication_has_exact_not_applicable_shape(self) -> None:
        repository = "https://github.com/rrnewton/hermit_test_ledger.git"
        publication = {
            "state": "not-applicable",
            "recorded_at": "2026-09-22T12:00:00Z",
            "repository": repository,
            "detail": "authoritative schema-5 zero-work no-result",
        }
        accepted = run_registry.parse_current_record(
            validation_handle(ledger_mirror=publication)
        )
        self.assertEqual(publication, accepted.raw["ledger_mirror"])

        for candidate in (
            {key: value for key, value in publication.items() if key != "detail"},
            {**publication, "extra": "not allowed"},
            {**publication, "repository": "https://example.invalid/ledger.git"},
            {**publication, "detail": ""},
        ):
            with self.subTest(candidate=candidate), self.assertRaises(RuntimeError):
                run_registry.parse_current_record(
                    validation_handle(ledger_mirror=candidate)
                )

    def test_modern_publication_preserves_the_complete_file_map(self) -> None:
        publication = modern_publication()
        accepted = run_registry.parse_current_record(validation_handle(ledger_mirror=publication))
        self.assertEqual(publication, accepted.raw["ledger_mirror"])
        files = publication["scorecard"]["files"]
        objects = [path for path in files if "/objects/" in path]
        # The registry checks report metadata. Equal bytes across the three
        # operand roles can share an object; the proof reader checks references.
        for removed in (objects[:1], objects[:2], list(set(files) - {
                "scorecard/SCORECARD.md", "scorecard/cells.json"})):
            with self.subTest(removed=removed):
                subset = {path: digest for path, digest in files.items() if path not in removed}
                candidate = {**publication, "scorecard": {**publication["scorecard"], "files": subset}}
                accepted = run_registry.parse_current_record(validation_handle(ledger_mirror=candidate))
                self.assertEqual(candidate, accepted.raw["ledger_mirror"])

    def test_modern_publication_refuses_malformed_or_unrelated_proof_groups(self) -> None:
        publication = modern_publication()
        files = publication["scorecard"]["files"]
        carrier = next(path for path in files if "/runs/" in path)
        objects = [path for path in files if "/objects/" in path]
        bad_files = {
            "missing generated markdown": {k: v for k, v in files.items() if k != "scorecard/SCORECARD.md"},
            "missing generated cells": {k: v for k, v in files.items() if k != "scorecard/cells.json"},
            "objects without carrier": {k: v for k, v in files.items() if k != carrier},
            "carrier without objects": {k: v for k, v in files.items() if k not in objects},
            "second carrier": {**files, f"scorecard/finalized/runs/{'a' * 64}.json": "b" * 64},
            "fourth object": {**files, f"scorecard/finalized/objects/sha256/{'a' * 64}": "a" * 64},
            "object identity mismatch": {**files, objects[0]: "0" * 64},
            "uppercase digest": {**files, "scorecard/cells.json": "A" * 64},
            "non-string digest": {**files, carrier: 1},
            "non-string path": {**files, 1: "a" * 64},
        }
        for path in (
            "scorecard/unrelated.json", "scorecard/finalized/runs/short.json",
            f"scorecard/finalized/runs/{'A' * 64}.json",
            f"scorecard/finalized/runs/{'a' * 64}.json/extra",
            f"scorecard/finalized/objects/sha256/{'a' * 64}.json",
            f"scorecard/finalized/objects/sha256/../{'a' * 64}",
            f"scorecard//finalized/objects/sha256/{'a' * 64}",
            f"/scorecard/finalized/objects/sha256/{'a' * 64}",
            f"scorecard\\finalized\\objects\\sha256\\{'a' * 64}",
        ):
            bad_files[path] = {**files, path: "a" * 64}
        for reason, candidate_files in bad_files.items():
            with self.subTest(reason=reason), self.assertRaisesRegex(
                RuntimeError, "validation-run-handle-ledger_mirror.scorecard.files"
            ):
                candidate = {**publication, "scorecard": {**publication["scorecard"], "files": candidate_files}}
                run_registry.parse_current_record(validation_handle(ledger_mirror=candidate))

    def test_refused_admission_requires_reason_and_exit_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-admission_result: refused must contain exactly",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    admission_result={
                        "state": "refused",
                        "recorded_at": "2026-08-28T07:00:01+00:00",
                    }
                )
            )

    def test_validation_service_result_schema_is_checked_when_present(self) -> None:
        historical = run_registry.parse_current_record(
            validation_handle(service_result_schema=1)
        )
        writeback = run_registry.parse_current_record(
            validation_handle(service_result_schema=2)
        )
        selection = run_registry.parse_current_record(
            validation_handle(service_result_schema=3)
        )
        counts = run_registry.parse_current_record(
            validation_handle(service_result_schema=4)
        )
        current = run_registry.parse_current_record(
            validation_handle(service_result_schema=5)
        )
        self.assertEqual(1, historical.raw["service_result_schema"])
        self.assertEqual(2, writeback.raw["service_result_schema"])
        self.assertEqual(3, selection.raw["service_result_schema"])
        self.assertEqual(4, counts.raw["service_result_schema"])
        self.assertEqual(5, current.raw["service_result_schema"])

        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-service_result_schema: expected 1, 2, 3, 4, or 5, got 6",
        ):
            run_registry.parse_current_record(
                validation_handle(service_result_schema=6)
            )

        for invalid in (True, 4.0, 5.0):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(
                RuntimeError,
                "validation-run-handle-service_result_schema",
            ):
                run_registry.parse_current_record(
                    validation_handle(service_result_schema=invalid)
                )

    def test_bench_failed_state_is_distinct_and_complete(self) -> None:
        handle = run_registry.parse_current_record(
            bench_handle(
                state="failed",
                result="failed",
                exit_code=1,
                finished_at="2026-08-28T07:01:00Z",
            )
        )
        self.assertEqual(run_registry.RunKind.BENCH, handle.kind)
        self.assertEqual(run_registry.RunState.FAILED, handle.state)
        self.assertFalse(handle.counts_as_validation)

    def test_validation_cannot_silently_adopt_bench_failed_state(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-state: 'failed' is not valid for kind 'validate'"
        ):
            run_registry.parse_current_record(validation_handle(state="failed"))

    def test_new_state_fails_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-state: unsupported value 'cancelled'"
        ):
            run_registry.parse_current_record(validation_handle(state="cancelled"))

    def test_missing_kind_fails_by_name(self) -> None:
        value = validation_handle()
        del value["kind"]
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-kind: must be a nonempty string"
        ):
            run_registry.parse_current_record(value)

    def test_unknown_field_fails_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-field: unknown field.*invented"
        ):
            run_registry.parse_current_record(validation_handle(invented=1))

    def test_completed_status_must_agree_with_result_and_exit(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "validation-run-handle-exit_code"):
            run_registry.parse_current_record(
                validation_handle(
                    state="completed",
                    result="success",
                    exit_code=1,
                    finished_at="2026-08-28T07:01:00Z",
                    final_validate_status="PASSED",
                )
            )

    def test_passed_validation_with_failed_writeback_keeps_both_facts(self) -> None:
        current = run_registry.parse_current_record(
            validation_handle(
                state="completed",
                result="success",
                exit_code=75,
                finished_at="2026-08-28T07:01:00Z",
                final_validate_status="PASSED",
                detail=None,
                service_result_schema=run_registry.service_result.SCHEMA_VERSION,
                selection_mode="full",
                executed_tests=2,
                passed_tests=2,
                scorecard_writeback={
                    "status": "failed",
                    "error": "fixture refusal",
                },
            )
        )
        counts = run_registry.parse_current_record(
            validation_handle(
                state="completed",
                result="success",
                exit_code=75,
                finished_at="2026-08-28T07:01:00Z",
                final_validate_status="PASSED",
                service_result_schema=(
                    run_registry.service_result.TEST_COUNTS_SCHEMA_VERSION
                ),
                selection_mode="full",
                executed_tests=2,
                passed_tests=2,
                scorecard_writeback={
                    "status": "failed",
                    "error": "fixture refusal",
                },
            )
        )
        for handle in (counts, current):
            self.assertEqual("PASSED", handle.raw["final_validate_status"])
            self.assertEqual(75, handle.raw["exit_code"])
            self.assertEqual(
                "failed", handle.raw["scorecard_writeback"]["status"]
            )

    def test_current_could_not_run_requires_explicit_detail(self) -> None:
        base = dict(
            state="completed",
            result="no-result",
            detail="launcher refused: missing KVM device",
            exit_code=75,
            finished_at="2026-08-28T07:01:00Z",
            final_validate_status="COULD_NOT_RUN",
            service_result_schema=run_registry.service_result.SCHEMA_VERSION,
            selection_mode=None,
            executed_nodes=0,
            executed_tests=None,
            passed_tests=None,
            scorecard_writeback=None,
        )
        handle = run_registry.parse_current_record(validation_handle(**base))
        self.assertEqual(
            "launcher refused: missing KVM device", handle.raw["detail"]
        )

        missing = validation_handle(**base)
        del missing["detail"]
        with self.assertRaisesRegex(
            RuntimeError,
            "detail: is required for a completed current validation result",
        ):
            run_registry.parse_current_record(missing)

        null = validation_handle(**base)
        null["detail"] = None
        with self.assertRaisesRegex(
            RuntimeError,
            "detail: must be a nonempty string",
        ):
            run_registry.parse_current_record(null)

    def test_current_completed_result_requires_a_named_writeback_value(self) -> None:
        base = dict(
            state="completed",
            result="success",
            exit_code=0,
            finished_at="2026-08-28T07:01:00Z",
            final_validate_status="PASSED",
            detail=None,
            service_result_schema=run_registry.service_result.SCHEMA_VERSION,
            selection_mode="full",
            executed_tests=2,
            passed_tests=2,
        )
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-scorecard_writeback: is required",
        ):
            run_registry.parse_current_record(validation_handle(**base))
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-scorecard_writeback: expected null",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    **base,
                    scorecard_writeback={"status": "invented"},
                )
            )

    def test_current_completed_result_requires_consistent_passed_count(self) -> None:
        base = dict(
            state="completed",
            result="failure",
            exit_code=1,
            finished_at="2026-08-28T07:01:00Z",
            final_validate_status="FAILED",
            detail=None,
            service_result_schema=run_registry.service_result.SCHEMA_VERSION,
            selection_mode="full",
            executed_tests=3,
            passed_tests=2,
            scorecard_writeback=None,
        )
        handle = run_registry.parse_current_record(validation_handle(**base))
        self.assertEqual(2, handle.raw["passed_tests"])

        missing = validation_handle(**base)
        del missing["passed_tests"]
        with self.assertRaisesRegex(
            RuntimeError,
            "passed_tests: is required for a completed current validation result",
        ):
            run_registry.parse_current_record(missing)

        for label, updates, message in (
            (
                "missing",
                {"passed_tests": None},
                "current result with executed_tests requires an exact count",
            ),
            ("greater", {"passed_tests": 4}, "exceeds executed_tests 3"),
            (
                "successful mismatch",
                {
                    "result": "success",
                    "exit_code": 0,
                    "final_validate_status": "PASSED",
                    "passed_tests": 2,
                },
                "requires passed_tests == executed_tests",
            ),
        ):
            with self.subTest(label=label):
                with self.assertRaisesRegex(RuntimeError, message):
                    run_registry.parse_current_record(
                        validation_handle(**{**base, **updates})
                    )

    def test_historical_completed_results_keep_passed_count_unknown(self) -> None:
        base = {
            "state": "completed",
            "result": "success",
            "exit_code": 0,
            "finished_at": "2026-08-28T07:01:00Z",
            "final_validate_status": "PASSED",
            "executed_tests": 3,
        }
        for version in (1, 2, 3):
            with self.subTest(version=version):
                historical = validation_handle(
                    **base,
                    service_result_schema=version,
                    **(
                        {"scorecard_writeback": {"status": "completed"}}
                        if version in (2, 3)
                        else {}
                    ),
                    **({"selection_mode": "full"} if version == 3 else {}),
                )
                parsed = run_registry.parse_current_record(historical)
                self.assertNotIn("passed_tests", parsed.raw)

                with self.assertRaisesRegex(
                    RuntimeError,
                    "passed_tests: must be null for a historical",
                ):
                    run_registry.parse_current_record(
                        {**historical, "passed_tests": 3}
                    )

    def test_required_validation_path_fails_by_name_when_empty(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-source_checkout"
        ):
            run_registry.parse_current_record(validation_handle(source_checkout=""))

    def test_malformed_process_identity_fails_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-process_identity.start_ticks"
        ):
            run_registry.parse_current_record(
                validation_handle(
                    state="running",
                    process_identity={"pid": 1, "start_ticks": 0, "boot_id": "boot"},
                )
            )

    def test_schema_one_remains_readable_without_current_semantics(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "legacy.json"
            legacy = {"schema_version": 1, "state": "cancelled", "old": "field"}
            path.write_text(json.dumps(legacy))
            self.assertEqual(legacy, run_registry.read_record(path))

    def test_canonical_run_reader_rejects_symlink_and_hardlink_handles(self) -> None:
        for mutation in ("symlink", "hardlink"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                path = root / "ignored/validate/runs/validate-linked.json"
                run_registry.create_current_record(path, validation_handle())
                external = root / f"outside-{mutation}.json"
                if mutation == "symlink":
                    path.rename(external)
                    path.symlink_to(external)
                else:
                    os.link(path, external)

                with self.assertRaisesRegex(
                    RuntimeError, "regular single-link"
                ):
                    run_registry.read_record(path)

    def test_historical_schema_one_kind_remains_readable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manual-bench.json"
            historical = {
                "schema_version": 1,
                "kind": "bench",
                "producer": "systemd-user-v1",
                "state": "completed",
                "purpose": "historical manual benchmark",
            }
            path.write_text(json.dumps(historical))
            self.assertEqual(historical, run_registry.read_record(path))

    def test_pending_owner_watch_correction_makes_run_handles_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            transaction = run_registry.owner_watch_correction_transaction(root)
            transaction.mkdir()

            with self.assertRaisesRegex(RuntimeError, "recovery is pending"):
                run_registry.read_record(path)
            with self.assertRaisesRegex(RuntimeError, "recovery is pending"):
                run_registry.inspect_current_records([path])

    def test_symlinked_owner_watch_correction_marker_is_also_pending(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            outside = root / "outside"
            outside.mkdir()
            transaction = run_registry.owner_watch_correction_transaction(root)
            transaction.symlink_to(outside, target_is_directory=True)

            with self.assertRaisesRegex(RuntimeError, "recovery is pending"):
                run_registry.read_record(path)

    def test_multi_record_inspection_returns_current_records(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            current = directory / "validate-test.json"
            run_registry.write_record(current, validation_handle(state="launching"))

            report = run_registry.inspect_current_records([current])

            self.assertEqual(
                [str(current)], [row["path"] for row in report["records"]]
            )
            self.assertEqual("validate", report["records"][0]["record"]["kind"])
            self.assertEqual([], report["unreadable"])

    def test_multi_record_inspection_isolates_one_bad_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            first = directory / "validate-first.json"
            malformed = directory / "validate-malformed.json"
            second = directory / "validate-second.json"
            first_value = validation_handle(state="launching")
            first_value["unit"] = "validate-first.service"
            second_value = validation_handle(state="running")
            second_value["unit"] = "validate-second.service"
            malformed_value = validation_handle(state="cancelled")
            malformed_value["unit"] = "validate-malformed.service"
            run_registry.write_record(first, first_value)
            malformed.write_text(json.dumps(malformed_value))
            run_registry.write_record(second, second_value)

            report = run_registry.inspect_current_records(
                [first, malformed, second]
            )

            self.assertEqual(
                [str(first), str(second)],
                [row["path"] for row in report["records"]],
            )
            self.assertEqual(
                [str(malformed)],
                [row["path"] for row in report["unreadable"]],
            )
            self.assertIn(
                "validation-run-handle-state",
                report["unreadable"][0]["error"],
            )

    def test_multi_record_inspection_does_not_isolate_population_unavailability(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "validate-test.json"
            path.write_text(json.dumps(validation_handle(state="running")))
            with mock.patch.object(
                run_registry,
                "read_record",
                side_effect=run_registry.OwnerWatchCorrectionUnavailable(
                    "owner-watch correction appeared during the record read"
                ),
            ):
                with self.assertRaisesRegex(
                    run_registry.OwnerWatchCorrectionUnavailable,
                    "appeared during the record read",
                ):
                    run_registry.inspect_current_records([path])

    def test_process_identity_cannot_be_rebound(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            first = run_registry.ProcessIdentity(1, 2, "boot-a")
            run_registry.bind_process_identity(path, first)
            self.assertEqual(1, run_registry.read_record(path)["process_identity"]["pid"])
            with self.assertRaisesRegex(RuntimeError, "different process identity"):
                run_registry.bind_process_identity(
                    path, run_registry.ProcessIdentity(3, 4, "boot-b")
                )

    def test_admission_result_cannot_be_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            with (
                mock.patch.object(
                    run_registry.socket,
                    "gethostname",
                    return_value="hosta.example.com",
                ),
                mock.patch.object(
                    run_registry,
                    "_ledger_run_numbers",
                    return_value=(700, 0),
                ),
            ):
                run_registry.publish_admission_result(
                    path, state=run_registry.AdmissionState.ADMITTED
                )
            self.assertEqual(
                "admitted", run_registry.read_record(path)["admission_result"]["state"]
            )
            with self.assertRaisesRegex(RuntimeError, "different admission result"):
                run_registry.publish_admission_result(
                    path,
                    state=run_registry.AdmissionState.REFUSED,
                    reason=run_registry.AdmissionRefusalReason.VALIDATION_LOCK_REFUSAL,
                    exit_code=3,
                )

    def test_run_number_is_assigned_while_record_is_launching_and_never_reused(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runs = root / "ignored/validate/runs"
            first = runs / "validate-first.json"
            second = runs / "validate-second.json"
            run_registry.create_current_record(
                first, validation_handle(state="launching", unit="validate-first.service")
            )
            run_registry.create_current_record(
                second,
                validation_handle(state="launching", unit="validate-second.service"),
            )
            with (
                mock.patch.object(
                    run_registry.socket,
                    "gethostname",
                    return_value="hosta.example.com",
                ),
                mock.patch.object(
                    run_registry,
                    "_ledger_run_numbers",
                    return_value=(700, 0),
                ) as ledger_numbers,
            ):
                assigned = run_registry.publish_admission_result(
                    first, state=run_registry.AdmissionState.ADMITTED
                )
                repeated = run_registry.publish_admission_result(
                    first, state=run_registry.AdmissionState.ADMITTED
                )
                next_assigned = run_registry.publish_admission_result(
                    second, state=run_registry.AdmissionState.ADMITTED
                )

            self.assertEqual("launching", assigned["state"])
            self.assertEqual(701, assigned["admission_result"]["run_number"])
            self.assertEqual(701, repeated["admission_result"]["run_number"])
            self.assertEqual(702, next_assigned["admission_result"]["run_number"])
            ledger_numbers.assert_called_once_with(
                root, team="hermit", host="hosta"
            )
            self.assertEqual(
                701,
                run_registry.read_record(first)["admission_result"]["run_number"],
            )
            projected = run_registry.projection(assigned)
            self.assertNotIn("run_number", projected)
            self.assertEqual(701, projected["admission_result"]["run_number"])

    def test_bench_admission_does_not_consume_a_validation_run_number(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "hermit-pressure-test.json"
            run_registry.write_record(path, bench_handle())

            admitted = run_registry.publish_admission_result(
                path, state=run_registry.AdmissionState.ADMITTED
            )

            self.assertEqual(
                {"state", "recorded_at"}, set(admitted["admission_result"])
            )

    def test_concurrent_admissions_receive_distinct_run_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runs = root / "ignored/validate/runs"
            paths = [runs / "validate-first.json", runs / "validate-second.json"]
            for index, path in enumerate(paths, 1):
                run_registry.create_current_record(
                    path,
                    validation_handle(
                        state="launching", unit=f"validate-{index}.service"
                    ),
                )
            with (
                mock.patch.object(
                    run_registry.socket,
                    "gethostname",
                    return_value="hosta.example.com",
                ),
                mock.patch.object(
                    run_registry,
                    "_ledger_run_numbers",
                    return_value=(700, 0),
                ) as ledger_numbers,
                ThreadPoolExecutor(max_workers=2) as executor,
            ):
                admitted = list(
                    executor.map(
                        lambda path: run_registry.publish_admission_result(
                            path, state=run_registry.AdmissionState.ADMITTED
                        ),
                        paths,
                    )
                )

            self.assertEqual(
                [701, 702],
                sorted(value["admission_result"]["run_number"] for value in admitted),
            )
            ledger_numbers.assert_called_once_with(
                root, team="hermit", host="hosta"
            )

    def test_cleanup_population_authority_rejects_cross_thread_writer(self) -> None:
        """A callback thread cannot wait forever for its caller's authority."""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-callback.json"
            run_registry.create_current_record(path, validation_handle())
            with run_registry.run_population_lock(
                root, reject_same_process=True
            ) as authority:
                with ThreadPoolExecutor(max_workers=1) as executor:
                    future = executor.submit(
                        run_registry.update_record,
                        path,
                        detail="must not wait",
                    )
                    with self.assertRaises(run_registry.RecordLocked):
                        future.result(timeout=1)
                # Explicitly delegated authority remains usable for internal
                # bookkeeping while the same record population is frozen.
                delegated = run_registry.update_record(
                    path,
                    _population_authority=authority,
                    detail="authority delegated",
                )
                self.assertEqual("authority delegated", delegated["detail"])

            run_registry.update_record(path, detail="authority released")
            self.assertEqual("authority released", run_registry.read_record(path)["detail"])

    def test_cleanup_population_authority_blocks_external_writer(self) -> None:
        """The process-local deadlock guard must not replace the OS boundary."""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-external.json"
            run_registry.create_current_record(path, validation_handle())
            program = """
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import run_registry
try:
    run_registry.update_record(Path(sys.argv[2]), blocking=False, detail='external')
except run_registry.RecordLocked:
    print('blocked')
else:
    raise SystemExit('external writer escaped population authority')
"""
            with run_registry.run_population_lock(
                root, reject_same_process=True
            ):
                child = subprocess.run(
                    [
                        sys.executable,
                        "-c",
                        program,
                        str(ROOT / "ci-hub" / "validate"),
                        str(path),
                    ],
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=2,
                )

            self.assertEqual(0, child.returncode, child.stderr)
            self.assertEqual("blocked", child.stdout.strip())
            self.assertNotIn("detail", run_registry.read_record(path))

    def test_cleanup_generation_authority_blocks_external_writer(self) -> None:
        """The rollout boundary is a kernel lock, not process-local state."""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-generation.json"
            run_registry.create_current_record(path, validation_handle())
            program = """
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import run_registry
try:
    run_registry.update_record(Path(sys.argv[2]), blocking=False, detail='external')
except run_registry.RecordLocked:
    print('blocked')
else:
    raise SystemExit('external writer escaped generation authority')
"""
            with run_registry.run_writer_generation_lock(
                root, exclusive=True, blocking=False
            ):
                child = subprocess.run(
                    [
                        sys.executable,
                        "-c",
                        program,
                        str(ROOT / "ci-hub" / "validate"),
                        str(path),
                    ],
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=2,
                )

            self.assertEqual(0, child.returncode, child.stderr)
            self.assertEqual("blocked", child.stdout.strip())
            self.assertNotIn("detail", run_registry.read_record(path))

    @unittest.skipUnless(hasattr(os, "fork"), "requires POSIX fork")
    def test_cleanup_population_authority_rejects_forked_writer(self) -> None:
        """A no-exec child fails closed instead of waiting on its inherited flock."""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-fork.json"
            run_registry.create_current_record(path, validation_handle())
            read_fd, write_fd = os.pipe()
            child: int | None = None
            try:
                with run_registry.run_population_lock(
                    root, reject_same_process=True
                ):
                    child = os.fork()
                    if child == 0:
                        os.close(read_fd)
                        try:
                            run_registry.update_record(path, detail="fork child")
                        except BaseException as error:
                            os.write(
                                write_fd,
                                f"error:{type(error).__name__}".encode(),
                            )
                        else:
                            os.write(write_fd, b"completed")
                        finally:
                            os._exit(0)
                    os.close(write_fd)
                    ready, _writable, _exceptional = select.select(
                        [read_fd], [], [], 1.0
                    )
                    self.assertEqual(
                        [read_fd], ready, "forked callback writer deadlocked"
                    )
                    message = os.read(read_fd, 1024).decode()
                    waited, status = os.waitpid(child, 0)
                    child = None
                    self.assertGreater(waited, 0)
                    self.assertEqual(0, os.waitstatus_to_exitcode(status))
                    self.assertEqual("error:RecordLocked", message)
            finally:
                if child is not None:
                    os.kill(child, 9)
                    os.waitpid(child, 0)
                with suppress(OSError):
                    os.close(read_fd)
                with suppress(OSError):
                    os.close(write_fd)
            self.assertNotIn("detail", run_registry.read_record(path))

    @unittest.skipUnless(hasattr(os, "fork"), "requires POSIX fork")
    def test_cleanup_generation_authority_rejects_forked_writer(self) -> None:
        """The outer cleanup lock also rejects a no-exec child without hanging."""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-generation-fork.json"
            run_registry.create_current_record(path, validation_handle())
            read_fd, write_fd = os.pipe()
            child: int | None = None
            try:
                with run_registry.run_writer_generation_lock(
                    root, exclusive=True
                ):
                    child = os.fork()
                    if child == 0:
                        os.close(read_fd)
                        try:
                            run_registry.update_record(
                                path, detail="generation fork child"
                            )
                        except BaseException as error:
                            os.write(
                                write_fd,
                                f"error:{type(error).__name__}".encode(),
                            )
                        else:
                            os.write(write_fd, b"completed")
                        finally:
                            os._exit(0)
                    os.close(write_fd)
                    ready, _writable, _exceptional = select.select(
                        [read_fd], [], [], 1.0
                    )
                    self.assertEqual(
                        [read_fd], ready, "forked generation writer deadlocked"
                    )
                    message = os.read(read_fd, 1024).decode()
                    waited, status = os.waitpid(child, 0)
                    child = None
                    self.assertGreater(waited, 0)
                    self.assertEqual(0, os.waitstatus_to_exitcode(status))
                    self.assertEqual("error:RecordLocked", message)
            finally:
                if child is not None:
                    os.kill(child, 9)
                    os.waitpid(child, 0)
                with suppress(OSError):
                    os.close(read_fd)
                with suppress(OSError):
                    os.close(write_fd)
            self.assertNotIn("detail", run_registry.read_record(path))

    def test_admitted_result_accepts_an_absent_run_number_but_checks_one_present(self) -> None:
        handle = run_registry.parse_current_record(
            validation_handle(
                admission_result={
                    "state": "admitted",
                    "recorded_at": "2026-08-28T07:00:01+00:00",
                }
            )
        )
        self.assertIsNotNone(handle.admission_result)
        self.assertIsNone(handle.admission_result.run_number)
        projected = run_registry.projection(handle.raw)
        self.assertNotIn("run_number", projected["admission_result"])

        for value in (0, -1, True, "1"):
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-admission_result.run_number"
                ):
                    run_registry.parse_current_record(
                        validation_handle(
                            admission_result={
                                "state": "admitted",
                                "recorded_at": "2026-08-28T07:00:01+00:00",
                                "run_number": value,
                            }
                        )
                    )

    def test_republishing_admitted_result_does_not_backfill_missing_run_number(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = (
                Path(temporary)
                / "ignored/validate/runs/validate-historical.json"
            )
            run_registry.create_current_record(
                path,
                validation_handle(
                    state="launching",
                    unit="validate-historical.service",
                    admission_result={
                        "state": "admitted",
                        "recorded_at": "2026-08-28T07:00:01+00:00",
                    },
                ),
            )

            repeated = run_registry.publish_admission_result(
                path, state=run_registry.AdmissionState.ADMITTED
            )

            self.assertEqual(
                {
                    "state": "admitted",
                    "recorded_at": "2026-08-28T07:00:01+00:00",
                },
                repeated["admission_result"],
            )

    def test_run_number_baseline_survives_an_unreadable_old_ledger_line(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shard = root / "ledger/hermit/hosta/2026-08.jsonl"
            shard.parent.mkdir(parents=True)
            events = [
                {
                    "event_type": "run.result",
                    "event_id": "first-result",
                    "run_id": "first-run",
                    "team": "hermit",
                    "host": "hosta",
                    "legacy_row": {},
                },
                {
                    "event_type": "run.correct",
                    "event_id": "first-correction",
                    "run_id": "first-run",
                    "team": "hermit",
                    "host": "hosta",
                },
                {
                    "event_type": "run.result",
                    "event_id": "duplicate-result",
                    "run_id": "first-run",
                    "team": "hermit",
                    "host": "hosta",
                    "legacy_row": {},
                },
                {
                    "event_type": "run.result",
                    "event_id": "second-result",
                    "run_id": "second-run",
                    "team": "hermit",
                    "host": "hosta.example.com",
                    "run_number": 9,
                    "legacy_row": {},
                },
                {
                    "event_type": "run.result",
                    "event_id": "fixture-result",
                    "run_id": "fixture-run",
                    "team": "hermit",
                    "host": "hosta",
                    "producer": {"tool": "ledger-live-sync-acceptance"},
                    "legacy_row": {},
                },
                {
                    "event_type": "run.result",
                    "event_id": "wrong-host-result",
                    "run_id": "wrong-host-run",
                    "team": "hermit",
                    "host": "wrong-host",
                    "legacy_row": {},
                },
            ]
            shard.write_text(
                "\n".join(json.dumps(event) for event in events[:2])
                + "\nnot-json\n"
                + "\n".join(json.dumps(event) for event in events[2:])
                + "\n"
            )

            self.assertEqual(
                (4, 9),
                run_registry._ledger_run_numbers_without_canonical_view(
                    root, team="hermit", host="hosta"
                ),
            )


class ReverieCountRegistryTests(unittest.TestCase):
    def test_exact_evidence_counts_failure_and_idempotent_replay(self) -> None:
        from test_reverie_validate import count_transport_fixture, count_evidence
        with tempfile.TemporaryDirectory() as temporary:
            path, record, binding = count_transport_fixture(Path(temporary))
            run_registry.create_record(path, record)
            proof = count_evidence(binding)
            first = run_registry.update_reverie_counts(path, record, evidence=proof)
            self.assertEqual((20, 19), (first["executed_tests"], first["passed_tests"]))
            self.assertEqual(first, run_registry.update_reverie_counts(path, record, evidence=proof))
            for key in ("exit_code", "result", "final_validate_status", "canonical_verdict", "wrapper_exit_code"):
                self.assertEqual(record[key], first[key])
            observed = run_registry.update_record(path, executed_tests=None, passed_tests=None)
            self.assertEqual((20, 19), (observed["executed_tests"], observed["passed_tests"]))
            self.assertIsNone(observed.get("executed_nodes"))

    def test_conflict_and_later_refusal_keep_original_proof_but_null_counts(self) -> None:
        from test_reverie_validate import count_transport_fixture, count_evidence
        import copy
        with tempfile.TemporaryDirectory() as temporary:
            path, record, binding = count_transport_fixture(Path(temporary))
            run_registry.create_record(path, record)
            first = count_evidence(binding)
            run_registry.update_reverie_counts(path, record, evidence=first)
            second = copy.deepcopy(binding)
            second["invocation_id"] = "4" * 32
            refused = run_registry.update_reverie_counts(path, record, evidence=count_evidence(second))
            self.assertIn("conflicting second", refused["reverie_count_refusal"])
            self.assertEqual(first, refused["reverie_count_evidence"])
            self.assertIsNone(refused["executed_tests"])
            self.assertIsNone(refused["passed_tests"])
            self.assertEqual(1, refused["exit_code"])
            retried = run_registry.update_reverie_counts(path, record, evidence=first)
            self.assertIsNone(retried["reverie_count_refusal"])
            self.assertEqual(19, retried["passed_tests"])
            unavailable = run_registry.update_reverie_counts(path, record, refusal="canonical checker unavailable")
            self.assertEqual(first, unavailable["reverie_count_evidence"])
            self.assertIsNone(unavailable["executed_tests"])

    def test_count_lock_contention_and_handle_generation_change_refuse(self) -> None:
        from test_reverie_validate import count_transport_fixture, count_evidence
        import copy
        with tempfile.TemporaryDirectory() as temporary:
            path, record, binding = count_transport_fixture(Path(temporary))
            run_registry.create_record(path, record)
            before = path.read_bytes()
            with run_registry.exclusive_record(path):
                with self.assertRaises(run_registry.RecordLocked):
                    run_registry.update_reverie_counts(path, record, evidence=count_evidence(binding))
            self.assertEqual(before, path.read_bytes())
            wrong = copy.deepcopy(record)
            wrong["process_identity"]["start_ticks"] += 1
            with self.assertRaisesRegex(RuntimeError, "identity changed"):
                run_registry.update_reverie_counts(path, wrong, evidence=count_evidence(binding))
            self.assertEqual(before, path.read_bytes())

    def test_no_evidence_no_counts_and_other_repository_cannot_borrow_marker(self) -> None:
        from test_reverie_validate import count_transport_fixture
        with tempfile.TemporaryDirectory() as temporary:
            _path, record, _binding = count_transport_fixture(Path(temporary))
            run_registry.parse_current_record(record)
            for counts in ({"executed_tests": 0}, {"passed_tests": 0}):
                with self.assertRaises(RuntimeError):
                    run_registry.parse_current_record({**record, **counts})
            legacy = {key: value for key, value in record.items() if key != "reverie_count_request"}
            self.assertIsNone(run_registry.parse_current_record(legacy).raw.get("passed_tests"))
            with self.assertRaisesRegex(RuntimeError, "only for Reverie"):
                run_registry.parse_current_record(validation_handle(reverie_count_request=record["reverie_count_request"]))


if __name__ == "__main__":
    unittest.main()
