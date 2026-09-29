# (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

from __future__ import annotations

import fcntl
import functools
import hashlib
import importlib.util
import json
import os
import re
import shlex
import stat
import subprocess
import tempfile
import time
import types
import unittest
import uuid
from collections.abc import Callable
from contextlib import ExitStack
from dataclasses import replace
from pathlib import Path
from typing import Any, ParamSpec, TypeVar
from unittest.mock import MagicMock, patch

import click
import testinfra.tfa.hermit as hermit_module
import testinfra.tfa.manifest as manifest_module
import testinfra.tfa.pipeline as pipeline_module
import testinfra.tfa.process as process_module
import testinfra.tfa.relocation as relocation_module
from click.testing import CliRunner
from security.frameworks.python.exec.subprocess import TrustedSubprocessWithList
from testinfra.tfa.case_checkout import CaseCheckoutError, validate_case_checkout
from testinfra.tfa.comments import (
    assert_bot_authorization,
    CommentError,
    final_text,
    initial_text,
    marker,
    plan_comment,
    post_comment,
)
from testinfra.tfa.drafts import (
    create_draft,
    DraftError,
    DraftMutation,
    DraftReadback,
    readback_draft,
    validate_draft_plan,
    validate_draft_readback,
)
from testinfra.tfa.hermit import (
    _pin_safehermit,
    _pin_safehermit_runtime,
    controller_evidence,
    hermit_attempt_argv,
    hermit_witness_argv,
    HermitError,
    packaged_hermit_argv,
    PACKAGED_HERMIT_RESOURCE_SHA256,
    PACKAGED_HERMIT_REVISION,
    safehermit_evidence,
    safehermit_runtime_paths,
    safehermit_wrapper_path,
)
from testinfra.tfa.hints import difference_hints, HintError
from testinfra.tfa.manifest import (
    Case,
    EXPECTED_WORKING_DIRECTORIES,
    load_manifest,
    Manifest,
    ManifestError,
)
from testinfra.tfa.orchestrator import (
    exercise_dry_run,
    run_investigation,
    run_seed,
    run_witness,
)
from testinfra.tfa.pipeline import (
    _diagnose_candidate_locked,
    _gtest_fatal_execution_evidence,
    _matches_failure_class,
    _revalidate_draft,
    _validation_result,
    _write_validation_seal,
    diagnose_candidate,
    independent_validate,
    run_case,
    validate_witness_artifacts,
    ValidationResult,
)
from testinfra.tfa.process import (
    _ProcessCgroup,
    codex_arguments,
    codex_review_arguments,
    CONTAINMENT,
    diagnostic_environment,
    payload_runtime_identity,
    positive_finite_timeout,
    ProcessError,
    ProcessResult,
    publish_codex_report,
    run_process,
    session_id_from_jsonl,
)
from testinfra.tfa.prompts import render_prompt
from testinfra.tfa.relocation import RelocationError, validated_relocation
from testinfra.tfa.report import (
    CandidateSummary,
    ReportError,
    ReportSummary,
    seal_candidate_report,
    validate_candidate_report,
    validate_qualified_report,
    validate_report,
)
from testinfra.tfa.review import render_review_prompt, ReviewError, validate_review
from testinfra.tfa.state import (
    Lease,
    Ledger,
    regular_file_record,
    seal_directory_evidence,
    seal_working_directory,
    Stage,
    StateError,
    verify_working_directory,
    working_directory_lease,
    write_metadata,
)


def _argv(seed: int, case_id: str = "asyncssl") -> list[str]:
    value = _case(case_id, case_id == "asyncssl")
    argv = value["pass_argv"]
    if not isinstance(argv, list):
        raise AssertionError("canonical pass argv is not a list")
    result = list(argv)
    result[result.index("--sched-seed") + 1] = str(seed)
    return result


_UNIT_WORKING_DIRECTORY_LOCK = Path(
    "/tmp/tfa-phase2/unit-working-directory-fixture.json"
)
_P = ParamSpec("_P")
_R = TypeVar("_R")


def _serialized_unit_working_directory(
    function: Callable[_P, _R],
) -> Callable[_P, _R]:
    @functools.wraps(function)
    def locked(*args: _P.args, **kwargs: _P.kwargs) -> _R:
        with Lease(_UNIT_WORKING_DIRECTORY_LOCK, wait=True):
            return function(*args, **kwargs)

    return locked


def _mock_verify_space(attempt_root: Path) -> hermit_module._VerifySpace:
    path = attempt_root / "brs-tfa-verify-mock"
    return hermit_module._VerifySpace(
        path=path,
        verify_dir=path / "verify",
        disk_limit="8G",
        limit_bytes=8_000_000_000,
        mount_id=1,
        root_record={"kind": "directory", "path": str(path)},
        marker_record={
            "kind": "file",
            "path": str(path / ".BOUNDED_RUN_SPACE"),
        },
        create_receipt={"returncode": 0},
    )


def _bounded_process_fixture(
    root: Path,
    operation: str,
    argv: list[str],
    returncode: int,
    stdout: str,
    stderr: str,
) -> dict[str, object]:
    artifact = root / "bounded-space-control" / f"{operation}-123"
    artifact.mkdir(parents=True)
    stdout_path = artifact / "stdout.txt"
    stderr_path = artifact / "stderr.txt"
    stdout_path.write_text(stdout)
    stderr_path.write_text(stderr)
    metadata_path = artifact / "metadata.json"
    write_metadata(
        metadata_path,
        {
            "argv": argv,
            "cgroup": str(artifact / "removed-cgroup"),
            "containment": CONTAINMENT,
            "containment_empty": True,
            "cwd": str(root),
            "environment": "restricted",
            "returncode": returncode,
            "stderr": regular_file_record(stderr_path),
            "stdout": regular_file_record(stdout_path),
            "timed_out": False,
            "timeout_seconds": hermit_module._BOUNDED_SPACE_TIMEOUT_SECONDS,
        },
    )
    return regular_file_record(metadata_path)


def _write_safehermit_fixture(
    root: Path,
    argv: tuple[str, ...],
    returncode: int,
    hermit_sha256: str,
) -> tuple[dict[str, object], tuple[str, ...]]:
    write_metadata(root / "execution-inputs.json", _pin_safehermit_runtime(root))
    allocation_key = hermit_module._verify_allocation_key(root)
    allocation_tag = hermit_module._verify_allocation_tag(root)
    log_root = hermit_module._safehermit_log_root(root)
    bounded_path = log_root / f"brs-{allocation_tag}-123-456"
    verify_log_dir = bounded_path / "verify"
    argv = hermit_module._replace_verify_log_dir(argv, root / "verify", verify_log_dir)
    copied_root = root / "verify"
    copied_root.mkdir()
    (copied_root / "summary.txt").write_text("verified\n")
    copied_output = hermit_module._verify_output_inventory(copied_root)
    bounded_run_space = safehermit_runtime_paths(root)[1]
    disk_limit = argv[argv.index("--sh-disk-limit") + 1]
    limit_bytes = 8_000_000_000
    tree_record = {
        "device": 1,
        "inode": 2,
        "kind": "directory",
        "mode": 0o755,
        "path": str(bounded_path),
    }
    marker_record = {
        "kind": "file",
        "mode": 0o644,
        "path": str(bounded_path / ".BOUNDED_RUN_SPACE"),
        "sha256": "0" * 64,
        "size": 1,
    }
    create_argv = [
        str(bounded_run_space),
        "create",
        "--limit",
        disk_limit,
        "--tag",
        allocation_tag,
        "--base",
        str(log_root),
    ]
    report_argv = [str(bounded_run_space), "report", str(bounded_path)]
    destroy_argv = [str(bounded_run_space), "destroy", str(bounded_path)]
    create_stdout = f"{bounded_path}\n"
    report_stdout = f"referenced=4096 exclusive=4096 limit={limit_bytes}\n"
    create_receipt = {
        "argv": create_argv,
        "process": _bounded_process_fixture(
            root, "create", create_argv, 0, create_stdout, ""
        ),
        "returncode": 0,
        "stderr": "",
        "stdout": create_stdout,
        "worker_pid": 123,
    }
    report_receipt = {
        "argv": report_argv,
        "exclusive_bytes": 4096,
        "limit_bytes": limit_bytes,
        "process": _bounded_process_fixture(
            root, "report", report_argv, 0, report_stdout, ""
        ),
        "referenced_bytes": 4096,
        "returncode": 0,
        "stderr": "",
        "stdout": report_stdout,
        "worker_pid": 124,
    }
    destroy_receipt = {
        "argv": destroy_argv,
        "process": _bounded_process_fixture(root, "destroy", destroy_argv, 0, "", ""),
        "returncode": 0,
        "stderr": "",
        "stdout": "",
        "worker_pid": 125,
    }
    verify_output: dict[str, object] = {
        "allocation_key": allocation_key,
        "bounded_tree": tree_record,
        "copied_output": copied_output,
        "copied_output_root": str(copied_root),
        "create": create_receipt,
        "destroy": destroy_receipt,
        "disk_limit": disk_limit,
        "limit_bytes": limit_bytes,
        "marker": marker_record,
        "mount_id": 1,
        "phase": "finalized",
        "report": report_receipt,
        "schema": "tfa.hermit.verify-output.v1",
        "verify_log_dir": str(verify_log_dir),
    }
    write_metadata(root / "verify-output.json", verify_output)
    write_metadata(
        root / "verify-output-transaction.json",
        {
            "allocation_key": allocation_key,
            "bounded_tree": tree_record,
            "copied_output": copied_output,
            "create": verify_output["create"],
            "destroy": verify_output["destroy"],
            "disk_limit": disk_limit,
            "final_evidence": regular_file_record(root / "verify-output.json"),
            "limit_bytes": limit_bytes,
            "marker": marker_record,
            "mount_id": 1,
            "phase": "finalized",
            "report": verify_output["report"],
            "schema": "tfa.hermit.verify-output-transaction.v1",
            "verify_log_dir": str(verify_log_dir),
        },
    )
    report = Path(argv[argv.index("--sh-report") + 1])
    run_id = f"{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}-12345"
    log_dir = log_root / f"brs-safehermit-{run_id}-123-456"
    deadline = argv[argv.index("--sh-deadline") + 1]
    max_bytes = argv[argv.index("--sh-max-log-bytes") + 1]
    binary = argv[argv.index("--sh-disk-limit") + 2]
    fields = [
        ("run_id", run_id),
        ("binary", binary),
        ("binary_source", "argv:positional-path"),
        ("binary_sha256", hermit_sha256),
        ("log_dir", str(log_dir)),
        ("log_file", str(log_dir / "hermit.log")),
        ("unit", f"safehermit-{run_id}"),
        ("bound.wall", f"APPLIED:{deadline}s"),
        ("bound.cgroup", "APPLIED:MemoryMax=16G MemorySwapMax=0"),
        (
            "bound.disk",
            f"APPLIED:{disk_limit} for log_dir only; guest paths and explicit "
            "log files elsewhere are outside this bound",
        ),
        (
            "bound.bytes",
            f"APPLIED:{max_bytes} (LETHAL: the run is cgroup-killed at the cap)",
        ),
        (
            "bound.logfilter",
            "NOT_APPLIED:by owner ruling -- the emitter is left loud so a stuck "
            "run stays visible; the low byte cap is the mechanism that fires",
        ),
        ("env.forwarded", "APPLIED:1 variables forwarded"),
        (
            "note.run_record",
            f"{log_root / 'registry' / f'{run_id}.json'} "
            "(best-effort; nothing depends on it)",
        ),
        ("elapsed_secs", "1"),
        ("bytes_written", "100"),
        ("truncated", "false"),
        ("runner_exit_code", "1" if returncode == 134 else str(returncode)),
        (
            "unit_result",
            "success"
            if returncode == 0
            else ("core-dump" if returncode == 134 else "exit-code"),
        ),
    ]
    if returncode != 0:
        fields.extend(
            (
                ("exec_main_pid", "12345"),
                ("exec_main_code", "3" if returncode == 134 else "1"),
                ("exec_main_status", "6" if returncode == 134 else str(returncode)),
                ("exec_main_start_monotonic_usec", "123456"),
            )
        )
    if returncode == 134:
        fields.append(("process_signal", "6"))
    fields.append(("exit_code", str(returncode)))
    report.write_text("".join(f"safehermit: {key}={value}\n" for key, value in fields))
    report_mtime = report.stat().st_mtime_ns
    started = report_mtime - 1
    finished = report_mtime + 1
    metadata: dict[str, object] = {
        "argv": list(argv),
        "finished_wall_time_ns": finished,
        "environment": "restricted",
        "payload_pid": 12345,
        "returncode": returncode,
        "started_wall_time_ns": started,
        "timed_out": False,
    }
    wrapper_dir = root / "wrapper"
    wrapper_dir.mkdir(exist_ok=True)
    write_metadata(wrapper_dir / "metadata.json", metadata)
    return metadata, argv


def _case(case_id: str = "asyncssl", live_task: bool = True) -> dict[str, object]:
    manifest = json.loads(
        Path(manifest_module.__file__).with_name("cases.v1.json").read_text()
    )
    cases = manifest.get("cases")
    if not isinstance(cases, list):
        raise AssertionError("canonical manifest cases are not a list")
    for value in cases:
        if isinstance(value, dict) and value.get("case_id") == case_id:
            result = json.loads(json.dumps(value))
            if result.get("live_task") is not live_task:
                raise AssertionError("test requested the wrong live-task contract")
            return result
    raise AssertionError(f"missing canonical case: {case_id}")


def _manifest(path: Path) -> None:
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "cases": [
                    _case("httpcoro", False),
                    _case("thrift", False),
                    _case("asyncssl", True),
                ],
            }
        )
    )


def _loaded_case() -> Case:
    with tempfile.TemporaryDirectory() as temporary:
        path = Path(temporary) / "cases.json"
        _manifest(path)
        return load_manifest(path).case("asyncssl")


def _assert_socket_isolation_blocker_text(
    test: unittest.TestCase, blocker: object
) -> None:
    # Independent of the production constant: the recorded blocker must name
    # both upstream fixes and every mount alias that needs them.
    if not isinstance(blocker, str):
        raise AssertionError("socket-isolation blocker is not text")
    for required in (
        "7cec3e110ce6dc5c14704dfa06f6db9c23a530fc",
        "a708fc683934d7d7ea7560354968fedd2be28581",
        "/tmp",
        "/var/tmp",
        "/data/tmpvol",
    ):
        test.assertIn(required, blocker)


def _approving_goalpost_assessment() -> dict[str, str]:
    return {
        "assertion_weakening": "NOT_FOUND",
        "tolerance_exemption_skip_comparator_relaxation": "NOT_FOUND",
        "failure_relabeling": "NOT_FOUND",
        "deleted_check_coverage": "PRESERVED",
    }


class ManifestTest(unittest.TestCase):
    def test_accepts_exact_three_case_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "cases.json"
            _manifest(path)
            manifest = load_manifest(path)
            self.assertEqual(
                [case.case_id for case in manifest.cases],
                ["httpcoro", "thrift", "asyncssl"],
            )

    def test_imported_hermit_blockers_preserve_historical_witness(self) -> None:
        value = json.loads(
            Path(manifest_module.__file__)
            .with_name("hermit-import-compatibility.v1.json")
            .read_text()
        )
        self.assertEqual(value["schema_version"], 1)
        self.assertEqual(
            value["upstream_revision"], hermit_module.PACKAGED_HERMIT_REVISION
        )
        self.assertEqual(value["resource"], "hermit-test-bin")
        self.assertEqual(
            value["qualification_probe"]["sha256"],
            hermit_module.PACKAGED_HERMIT_QUALIFICATION_SHA256,
        )
        self.assertEqual(value["historical_witness"]["state"], "PRESERVED")
        self.assertEqual(value["qualification_probe"]["state"], "BLOCKED")
        self.assertEqual(
            value["packaged_executable"]["observed_blockers"],
            [
                "--verify-verbose rejects /bin/true because Hermit host/runtime "
                "TRACE fields such as CPU identity, elapsed timing, and "
                "async-resource ordering differ between run 1 and run 2"
            ],
        )
        self.assertEqual(value["packaged_executable"]["state"], "BLOCKED")
        receipt_path = Path(manifest_module.__file__).with_name(
            value["qualification_probe"]["receipts_resource"]
        )
        self.assertEqual(
            hashlib.sha256(receipt_path.read_bytes()).hexdigest(),
            value["qualification_probe"]["receipts_sha256"],
        )
        receipts = json.loads(receipt_path.read_text())
        self.assertEqual(
            receipts["probe_binary"],
            {
                "build_mode": value["qualification_probe"]["mode"],
                "configured_target": value["qualification_probe"]["configured_target"],
                "sha256": value["qualification_probe"]["sha256"],
                "upstream_revision": value["upstream_revision"],
            },
        )
        candidates = value["qualification_probe"]["short_candidates"]
        self.assertEqual(
            [
                (candidate["case_id"], candidate["seed"], candidate["returncode"])
                for candidate in candidates
            ],
            [("asyncssl", 1, 1), ("thrift", 0, 132), ("httpcoro", 1, 1)],
        )
        self.assertEqual(len(value["qualification_probe"]["blockers"]), 3)
        self.assertEqual(
            [
                (receipt["case_id"], receipt["seed"], receipt["returncode"])
                for receipt in receipts["probes"]
            ],
            [("asyncssl", 1, 1), ("thrift", 0, 132), ("httpcoro", 1, 1)],
        )
        for receipt in receipts["probes"]:
            self.assertEqual(
                hashlib.sha256(
                    (json.dumps(receipt["argv"], separators=(",", ":")) + "\n").encode()
                ).hexdigest(),
                receipt["argv_sha256"],
            )
            self.assertEqual(
                hashlib.sha256(receipt["stdout"].encode()).hexdigest(),
                receipt["stdout_sha256"],
            )
            self.assertEqual(
                hashlib.sha256(receipt["stderr"].encode()).hexdigest(),
                receipt["stderr_sha256"],
            )
            self.assertEqual(
                hashlib.sha256((receipt["result_receipt"] + "\n").encode()).hexdigest(),
                receipt["result_receipt_sha256"],
            )
            result = json.loads(receipt["result_receipt"])
            self.assertEqual(result["argv"], receipt["argv"])
            self.assertEqual(result["case_id"], receipt["case_id"])
            self.assertEqual(result["returncode"], receipt["returncode"])
            self.assertEqual(
                hashlib.sha256(receipt["safehermit_report"].encode()).hexdigest(),
                receipt["safehermit_report_sha256"],
            )

    def test_rejects_generalized_population(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "cases.json"
            path.write_text(json.dumps({"schema_version": 1, "cases": [_case()]}))
            with self.assertRaisesRegex(ManifestError, "exactly three"):
                load_manifest(path)

    def test_rejects_witness_seed_in_fresh_grid(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "cases.json"
            cases = [
                _case("httpcoro", False),
                _case("thrift", False),
                _case("asyncssl", True),
            ]
            cases[0]["fresh_stress_seeds"] = [1, 3]
            path.write_text(json.dumps({"schema_version": 1, "cases": cases}))
            with self.assertRaisesRegex(ManifestError, "exclude both witness"):
                load_manifest(path)

    def test_rejects_missing_working_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "cases.json"
            cases = [
                _case("httpcoro", False),
                _case("thrift", False),
                _case("asyncssl", True),
            ]
            cases[0].pop("working_directory")
            path.write_text(json.dumps({"schema_version": 1, "cases": cases}))
            with self.assertRaisesRegex(ManifestError, "working_directory"):
                load_manifest(path)

    def test_rejects_working_directory_override(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "cases.json"
            cases = [
                _case("httpcoro", False),
                _case("thrift", False),
                _case("asyncssl", True),
            ]
            cases[2]["working_directory"] = "/tmp/override"
            path.write_text(json.dumps({"schema_version": 1, "cases": cases}))
            with self.assertRaisesRegex(ManifestError, "sealed"):
                load_manifest(path)

    def test_rejects_unverified_evidence_url(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "cases.json"
            cases = [
                _case("httpcoro", False),
                _case("thrift", False),
                _case("asyncssl", True),
            ]
            cases[1]["evidence_url"] = "https://example.com/unverified"
            path.write_text(json.dumps({"schema_version": 1, "cases": cases}))
            with self.assertRaisesRegex(ManifestError, "exact landed Git object URL"):
                load_manifest(path)

    def test_rejects_identity_or_exact_argv_drift(self) -> None:
        for field in ("test_id", "source_revision", "pass_argv"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / "cases.json"
                cases = [
                    _case("httpcoro", False),
                    _case("thrift", False),
                    _case("asyncssl", True),
                ]
                if field == "pass_argv":
                    argv = cases[1][field]
                    self.assertIsInstance(argv, list)
                    argv.append("--identity-drift")
                else:
                    cases[1][field] = "identity-drift"
                path.write_text(json.dumps({"schema_version": 1, "cases": cases}))
                with self.assertRaisesRegex(ManifestError, "canonical witness"):
                    load_manifest(path)

    def test_rejects_unretained_or_ambiguous_verify_output(self) -> None:
        for mutation in ("missing-keep", "duplicate-dir"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temp:
                path = Path(temp) / "cases.json"
                cases = [
                    _case("httpcoro", False),
                    _case("thrift", False),
                    _case("asyncssl", True),
                ]
                argv = cases[0]["pass_argv"]
                self.assertIsInstance(argv, list)
                if mutation == "missing-keep":
                    argv.remove("--keep-logs")
                else:
                    boundary = argv.index("--")
                    argv.insert(boundary, "--verify-log-dir=/tmp/duplicate")
                path.write_text(json.dumps({"schema_version": 1, "cases": cases}))
                with self.assertRaisesRegex(
                    ManifestError, "retained absolute verification"
                ):
                    load_manifest(path)


class PromptTest(unittest.TestCase):
    def test_post_fix_replay_is_confined_to_case_checkout(self) -> None:
        prompt = render_prompt(
            _loaded_case(),
            Path("/tmp/tfa-phase2/experiment/asyncssl"),
            Path("/tmp/tfa-phase2/cases/asyncssl"),
        )
        self.assertIn(
            "rewrite both the guest ELF path and same-path buck-out bind mount",
            prompt,
        )
        self.assertIn(
            "Never copy, replace, or link an ELF or dependency into",
            prompt,
        )
        self.assertIn(
            "Use only the controller commands below",
            prompt,
        )
        self.assertNotIn("/home/newton/work/dev-hermit/bin/safehermit", prompt)
        self.assertNotIn("--verify-strict", prompt)
        self.assertNotIn("--keep-logs", prompt)
        self.assertNotIn("--verify-log-dir", prompt)


class StateTest(unittest.TestCase):
    def test_lease_recovery_is_serialized_by_kernel_lock(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "lease.json"
            guard = path.with_name(f".{path.name}.lock")
            descriptor = os.open(guard, os.O_RDWR | os.O_CREAT, 0o600)
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                with self.assertRaisesRegex(StateError, "already held"):
                    Lease(path).acquire()
            finally:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
                os.close(descriptor)

    def test_transitions_and_recovery_are_linear_and_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            ledger = Ledger(Path(temporary) / "ledger.jsonl")
            first = ledger.append(Stage.VALIDATED_REPRO, {"ok": True})
            repeated = ledger.append(Stage.VALIDATED_REPRO, {"ignored": True})
            ledger.append(Stage.INITIAL_COMMENT, {"state": "artifact-only"})
            self.assertEqual(first, repeated)
            self.assertEqual(
                [entry.stage for entry in ledger.read()],
                [Stage.VALIDATED_REPRO, Stage.INITIAL_COMMENT],
            )

    def test_rejects_skipped_transition(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            ledger = Ledger(Path(temporary) / "ledger.jsonl")
            with self.assertRaisesRegex(StateError, "expected VALIDATED_REPRO"):
                ledger.append(Stage.INVESTIGATING, {})

    def test_atomic_lease_refuses_second_owner(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "lease.json"
            with Lease(path):
                with self.assertRaisesRegex(StateError, "already held"):
                    Lease(path).acquire()
            self.assertFalse(path.exists())

    def test_dead_owner_lease_is_recovered_atomically(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "lease.json"
            path.write_text(
                json.dumps(
                    {
                        "created_at": 0,
                        "hostname": os.uname().nodename,
                        "pid": 2_147_483_647,
                        "process_start": "missing",
                        "token": "dead",
                    }
                )
            )
            with Lease(path):
                recovered = list(path.parent.glob("lease.json.dead-*.json"))
                self.assertEqual(len(recovered), 1)
            self.assertFalse(path.exists())

    def test_malformed_crash_partial_lease_is_recovered(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "lease.json"
            path.write_text('{"pid":')
            with Lease(path):
                recovered = list(path.parent.glob("lease.json.dead-*.json"))
                self.assertEqual(len(recovered), 1)
                self.assertEqual(json.loads(path.read_text())["pid"], os.getpid())
            self.assertFalse(path.exists())

    def test_working_directory_must_be_empty_then_remain_sealed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            working = root / "cwd"
            seal = root / "seal.json"
            seal_working_directory(working, seal)
            verify_working_directory(working, seal)
            (working / "unexpected").write_text("changed")
            with self.assertRaisesRegex(StateError, "differs from its seal"):
                verify_working_directory(working, seal)

    def test_deleted_working_directory_root_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            working = root / "cwd"
            seal = root / "seal.json"
            seal_working_directory(working, seal)
            working.rmdir()
            with self.assertRaisesRegex(StateError, "root"):
                verify_working_directory(working, seal)

    def test_symlink_substituted_working_directory_root_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            working = root / "cwd"
            seal = root / "seal.json"
            seal_working_directory(working, seal)
            working.rmdir()
            replacement = root / "replacement"
            replacement.mkdir()
            working.symlink_to(replacement, target_is_directory=True)
            with self.assertRaisesRegex(StateError, "root"):
                verify_working_directory(working, seal)

    def test_working_directory_root_mode_drift_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            working = root / "cwd"
            seal = root / "seal.json"
            seal_working_directory(working, seal)
            working.chmod(0o700)
            with self.assertRaisesRegex(StateError, "differs from its seal"):
                verify_working_directory(working, seal)

    def test_replacement_working_directory_root_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            working = root / "cwd"
            seal = root / "seal.json"
            seal_working_directory(working, seal)
            original = root / "original"
            working.rename(original)
            working.mkdir(mode=original.stat().st_mode & 0o7777)
            with self.assertRaisesRegex(StateError, "differs from its seal"):
                verify_working_directory(working, seal)

    def test_dirty_unsealed_working_directory_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            working = root / "cwd"
            working.mkdir()
            (working / "unexpected").write_text("present")
            with self.assertRaisesRegex(StateError, "fresh and empty"):
                seal_working_directory(working, root / "seal.json")

    def test_case_working_directory_is_locked_across_campaigns(self) -> None:
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch("testinfra.tfa.state.WORKING_DIRECTORY_LOCK_ROOT", Path(temporary)),
        ):
            with working_directory_lease("asyncssl"):
                with self.assertRaisesRegex(StateError, "already held"):
                    working_directory_lease("asyncssl").acquire()


class CaseCheckoutTest(unittest.TestCase):
    def test_rejects_base_different_from_recorded_source(self) -> None:
        case = _loaded_case()
        with self.assertRaisesRegex(CaseCheckoutError, "base differs"):
            validate_case_checkout(case, "different", (), True)

    def test_rejects_outgoing_tfa_implementation_path(self) -> None:
        case = _loaded_case()
        with self.assertRaisesRegex(CaseCheckoutError, "implementation paths"):
            validate_case_checkout(
                case,
                case.source_revision,
                ("fbcode/testinfra/tfa/main.py",),
                False,
            )

    def test_accepts_case_only_change_on_exact_base(self) -> None:
        case = _loaded_case()
        state = validate_case_checkout(
            case,
            case.source_revision,
            ("fbcode/folly/io/async/AsyncSSLSocket.cpp",),
            False,
        )
        self.assertEqual(state.base_revision, case.source_revision)


class CommentTest(unittest.TestCase):
    def test_idempotent_marker_deduplicates(self) -> None:
        case = _loaded_case()
        text = initial_text(case)
        plan = plan_comment(case, "initial", text, json.dumps([{"text": text}]))
        self.assertTrue(plan.duplicate)
        self.assertIn(marker(case, "initial"), plan.text)

    def test_initial_comment_without_issue_is_artifact_only(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "cases.json"
            _manifest(path)
            case = load_manifest(path).case("thrift")
        with self.assertRaisesRegex(CommentError, "exact IssueID"):
            initial_text(case)
        text = initial_text(case, artifact_only=True)
        self.assertIn("(artifact-only preview; not posted)", text)
        self.assertIn("IssueID: `none (no exact FLAKY issue)`", text)
        self.assertIn(shlex.join(case.pass_argv), text)
        self.assertIn(shlex.join(case.fail_argv), text)
        self.assertIn(
            "https://github.com/rrnewton/hermit/tree/"
            "4630ee2a61f93221a43e2baec0a15f34102a2c61",
            text,
        )
        self.assertNotIn("not posted", initial_text(_loaded_case()))

    def test_malformed_comment_list_fails_closed(self) -> None:
        case = _loaded_case()
        with self.assertRaisesRegex(CommentError, "malformed row"):
            plan_comment(case, "initial", "text", json.dumps([{"body": "wrong"}]))

    def test_bot_auth_refuses_human_or_unproven_act_as(self) -> None:
        with self.assertRaisesRegex(CommentError, "absent or unproven"):
            assert_bot_authorization(
                {
                    "service_user_fbid": "4533519350210414",
                    "act_as_authorized": False,
                    "principal_kind": "service",
                }
            )
        with self.assertRaisesRegex(CommentError, "human identity"):
            assert_bot_authorization(
                {
                    "service_user_fbid": "4533519350210414",
                    "act_as_authorized": True,
                    "principal_kind": "human",
                }
            )

    def test_post_lists_first_then_refuses_unproven_bot(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            listed = ProcessResult(
                ("meta",), 0, 0.1, False, root / "stdout", root / "stderr"
            )
            listed.stdout_path.write_text("[]\n")
            with patch(
                "testinfra.tfa.comments.run_process", return_value=listed
            ) as run:
                case = _loaded_case()
                with self.assertRaisesRegex(CommentError, "absent or unproven"):
                    post_comment(
                        case,
                        "initial",
                        initial_text(case),
                        root,
                        {
                            "service_user_fbid": "4533519350210414",
                            "act_as_authorized": False,
                            "principal_kind": "service",
                        },
                    )
            self.assertEqual(run.call_count, 1)


def _hint_record(
    root: Path,
    seed: int,
    oracle: str,
    failure_class: str | None,
    stdout: str,
    guest_stderr: str,
) -> None:
    evidence = root / "evidence"
    evidence.mkdir(parents=True, exist_ok=True)
    prefix = evidence / f"selection-{seed:03d}-sched-seed-{seed:020d}-replica-1"
    Path(f"{prefix}.result.json").write_text(
        json.dumps(
            {
                "seed": {"flag": "--sched-seed", "value": seed},
                "result": {
                    "diagnostic": "status=VALID; selected=1",
                    "exit_code": 0 if oracle == "PASS" else 134,
                    "failure_class_id": failure_class,
                    "signal": None if oracle == "PASS" else "SIGABRT",
                    "target_oracle": oracle,
                },
            }
        )
    )
    Path(f"{prefix}.stdout.txt").write_text(stdout)
    Path(f"{prefix}.stderr.txt").write_text(
        "Logs contain 10 | 10 messages total\n"
        if oracle == "PASS"
        else "Logs contain 7 | 7 messages total\n"
    )
    with open(f"{prefix}.stderr.txt", "a") as stream:
        stream.write(
            ":: Success: deterministic. Determinism verified.\n" + guest_stderr
        )


class HintTest(unittest.TestCase):
    def _records(self, root: Path, case: Case, fail_class: str | None = None) -> None:
        _hint_record(
            root,
            case.historical_pass_seed,
            "PASS",
            None,
            "[ RUN      ] T.t\n[       OK ] T.t (1 ms)\n",
            "I0921 21:44:39.202094    19 A.cpp:1] first\n"
            "I0921 21:44:39.302094    19 A.cpp:2] second 0x7fff1234\n",
        )
        _hint_record(
            root,
            case.historical_fail_seed,
            "FAIL",
            case.failure_class if fail_class is None else fail_class,
            "[ RUN      ] T.t\nfbcode/a/T.cpp:12: Failure\nExpected: 3\n\n"
            "[  FAILED  ] T.t (2 ms)\n",
            "I0921 21:44:38.202094    19 A.cpp:2] second 0x7fff9999\n"
            "I0921 21:44:38.302094    19 A.cpp:1] first\n"
            "F0921 21:44:38.402094    19 T.cpp:12] Check failed: x\n"
            "    @ 000000000002cf2b folly::symbolizer::signalHandler(int)\n"
            "    @ 00000000000444ac raise\n"
            "    @ 000000000000c28c google::LogMessage::Fail()\n"
            "    @ 0000000000eb36c2 T_t_Test::TestBody()\n"
            "    @ 000000000000b2d3 folly::EventBase::loop()\n",
        )

    def test_hints_are_deterministic_and_hash_bound(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._records(root, case)
            hints = json.loads(json.dumps(difference_hints(case, root)))
            self.assertEqual(hints, difference_hints(case, root))
            stdout = next((root / "evidence").glob("selection-011-*.stdout.txt"))
        self.assertEqual(
            hints["qualification"], "deterministic-text-difference; not a root cause"
        )
        self.assertEqual(
            hints["inputs"]["fail.stdout"]["sha256"],
            hashlib.sha256(
                b"[ RUN      ] T.t\nfbcode/a/T.cpp:12: Failure\nExpected: 3\n\n"
                b"[  FAILED  ] T.t (2 ms)\n"
            ).hexdigest(),
        )
        self.assertEqual(hints["inputs"]["fail.stdout"]["path"], str(stdout))
        fail = hints["fail"]
        self.assertEqual(fail["gtest_verdict"], "FAILED")
        self.assertEqual(hints["pass"]["gtest_verdict"], "OK")
        self.assertEqual(
            fail["failures"],
            [
                "fbcode/a/T.cpp:12: Failure\nExpected: 3",
                "F T.cpp:12] Check failed: x",
            ],
        )
        self.assertEqual(
            fail["top_frames"], ["T_t_Test::TestBody()", "folly::EventBase::loop()"]
        )
        self.assertTrue(fail["determinism_verified"])
        self.assertEqual(hints["verifier_count_delta"], {"messages total": -3})
        self.assertFalse(hints["guest_stderr_same_lines_reordered"])
        self.assertEqual(
            hints["guest_stderr_diff"][:6],
            [
                "--- pass",
                "+++ fail",
                "@@ -1,2 +1,8 @@",
                "+I A.cpp:2] second 0x?",
                " I A.cpp:1] first",
                "-I A.cpp:2] second 0x?",
            ],
        )
        self.assertFalse(hints["guest_stderr_diff_truncated"])

    def test_reordered_guest_lines_are_flagged(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _hint_record(root, case.historical_pass_seed, "PASS", None, "", "a\nb\n")
            _hint_record(
                root,
                case.historical_fail_seed,
                "FAIL",
                case.failure_class,
                "",
                "b\na\n",
            )
            hints = json.loads(json.dumps(difference_hints(case, root)))
        self.assertTrue(hints["guest_stderr_same_lines_reordered"])
        self.assertEqual(hints["fail"]["gtest_verdict"], "NO_VERDICT_AFTER_RUN")

    def test_refuses_record_with_wrong_failure_class(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._records(root, case, fail_class="some-other-failure")
            with self.assertRaisesRegex(HintError, "wrong failure class"):
                difference_hints(case, root)

    def test_refuses_record_not_bound_to_manifest_seed(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._records(root, case)
            result = next((root / "evidence").glob("selection-011-*.result.json"))
            record = json.loads(result.read_text())
            record["seed"]["value"] = 12
            result.write_text(json.dumps(record))
            with self.assertRaisesRegex(HintError, "not bound to seed 11"):
                difference_hints(case, root)

    def test_refuses_pass_record_with_fail_oracle(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._records(root, case)
            result = next((root / "evidence").glob("selection-001-*.result.json"))
            record = json.loads(result.read_text())
            record["result"]["target_oracle"] = "FAIL"
            result.write_text(json.dumps(record))
            with self.assertRaisesRegex(
                HintError, "pass result record has the wrong oracle"
            ):
                difference_hints(case, root)

    def test_refuses_missing_selection(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(HintError, "exactly one replica-1 record"):
                difference_hints(case, Path(temporary))


def _relocated_attempt(
    root: Path, case: Case, outcome: str, seed: int, label: str, oracle: str
) -> list[str]:
    source = list(case.pass_argv if outcome == "pass" else case.fail_argv)
    run = root / label
    run.mkdir(parents=True)
    # Unit temp roots may live under /tmp, which the validator refuses in argv.
    argv_root = Path("/home/tfa-relocation-fixture") / label
    cwd = argv_root / "cwd"
    old_mount = (
        f"--mount=type=bind,source={case.working_directory},"
        f"target={case.working_directory}"
    )
    argv = []
    for index, token in enumerate(source):
        previous = source[index - 1] if index else ""
        if previous == "--sh-report":
            argv.append(str(argv_root / "safehermit.txt"))
        elif previous == "--sched-seed":
            argv.append(str(seed))
        elif token.startswith("--verify-log-dir="):
            argv.append(f"--verify-log-dir={argv_root / 'verify'}")
        elif token == old_mount:
            argv += [
                f"--mount=type=bind,source={cwd},target={cwd}",
                f"--tmp={argv_root / 'guest-tmp'}",
            ]
        else:
            argv.append(token)
    identity = next(t for t in argv if t.startswith("--gtest_filter="))[15:]
    head = (
        f"Note: Google Test filter = {identity}\n"
        "[==========] Running 1 test from 1 test suite.\n"
        f"[ RUN      ] {identity}\n"
    )
    if oracle == "PASS":
        stdout = head + f"[       OK ] {identity} (1 ms)\n[  PASSED  ] 1 test.\n"
    else:
        stdout = (
            head + "a.cpp:1: Failure\nwhose size 3 doesn't match ByteEventType::ACK\n"
            f"[  FAILED  ] {identity} (1 ms)\n[  PASSED  ] 0 tests.\n"
            f"[  FAILED  ] 1 test, listed below:\n[  FAILED  ] {identity}\n"
        )
    stderr = ":: Success: deterministic. Determinism verified.\n"
    returncode = {"PASS": 0, "FAIL": 1, "CENSORED": 124}[oracle]
    deadline = int(argv[argv.index("--sh-deadline") + 1])
    report = f"safehermit: bound.wall=APPLIED:{deadline}s\n"
    if oracle == "CENSORED":
        stdout, stderr = "", ":: Run1...\n"
        report += (
            f"safehermit: elapsed_secs={deadline}\nsafehermit: unit_result=timeout\n"
        )
    else:
        report += "safehermit: elapsed_secs=5\nsafehermit: unit_result=success\n"
    (run / "stdout.txt").write_text(stdout)
    (run / "stderr.txt").write_text(stderr)
    (run / "safehermit.txt").write_text(
        report + f"safehermit: exit_code={returncode}\n"
    )
    receipt = {
        "argv": argv,
        "case_id": case.case_id,
        "classification": {"oracle": oracle, "returncode": returncode},
        "duration_seconds": deadline + 1.0 if oracle == "CENSORED" else 5.0,
        "inputs": {
            "hermit": {"sha256": case.hermit_sha256},
            "guest": {"sha256": case.artifact_sha256},
        },
        "schema": "tfa.relocated-reproducer-run.v2",
        "seed": seed,
    }
    for name, filename in (
        ("stdout", "stdout.txt"),
        ("stderr", "stderr.txt"),
        ("safehermit_report", "safehermit.txt"),
    ):
        path = run / filename
        receipt[name] = {
            "path": str(path),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
    (run / "receipt.json").write_text(json.dumps(receipt))
    return argv


def _relocation_summary(
    root: Path, case: Case, pass_seed: int | None = None, censored: int = 0
) -> Path:
    seeds = {
        "pass": case.pass_seed if pass_seed is None else pass_seed,
        "fail": case.fail_seed,
    }
    witness = {}
    for outcome, seed in seeds.items():
        labels = [f"seed-{seed:03d}-attempt-{n}" for n in range(1, 3 + censored)]
        for label in labels[:censored]:
            _relocated_attempt(root, case, outcome, seed, label, "CENSORED")
        argvs = [
            _relocated_attempt(root, case, outcome, seed, label, outcome.upper())
            for label in labels[censored:]
        ]
        witness[outcome] = {
            "argv": argvs[0],
            "censored_attempts": labels[:censored],
            "cwd": f"/home/tfa-relocation-fixture/{labels[censored]}/cwd",
            "decided_attempts": labels[censored:],
            "seed": seed,
            "state": "REPRODUCED",
        }
    path = root / "summary.json"
    path.write_text(
        json.dumps(
            {
                "case_id": case.case_id,
                "schema": "tfa.relocated-reproducer-summary.v2",
                "witness": witness,
            }
        )
    )
    return path


class RelocationTest(unittest.TestCase):
    def _edit(self, path: Path, change: Callable[[Any], None]) -> None:
        value = json.loads(path.read_text())
        change(value)
        path.write_text(json.dumps(value))

    def _rebind(self, run: Path) -> None:
        def change(value: Any) -> None:
            for name, filename in (
                ("stdout", "stdout.txt"),
                ("stderr", "stderr.txt"),
                ("safehermit_report", "safehermit.txt"),
            ):
                data = (run / filename).read_bytes()
                value[name]["sha256"] = hashlib.sha256(data).hexdigest()

        self._edit(run / "receipt.json", change)

    def test_initial_comment_leads_with_measured_relocation(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case)
            relocation = validated_relocation(case, summary)
            text = initial_text(case, True, relocation, "ab" * 32)
            digest = hashlib.sha256(summary.read_bytes()).hexdigest()
        self.assertEqual(relocation.summary_sha256, digest)
        self.assertEqual(relocation.failing.decided_attempts[0], "seed-011-attempt-1")
        current = text.index("Current non-`/tmp` reproducers")
        historical = text.index("Historical phase-one record")
        self.assertLess(current, historical)
        self.assertLess(text.index(shlex.join(relocation.passing.argv)), historical)
        self.assertGreater(text.index(shlex.join(case.pass_argv)), historical)
        self.assertGreater(text.index(shlex.join(case.fail_argv)), historical)
        self.assertNotIn("/tmp/", text[:historical])
        self.assertIn(f"`experiment.json` sha256 `{'ab' * 32}`", text)
        self.assertIn(case.evidence_url, text)
        self.assertIn(digest, text)
        self.assertNotIn("Relocation changed", text)
        self.assertNotIn("CENSORED", text)

    def test_changed_seed_and_censored_attempts_are_reported(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case, pass_seed=0, censored=1)
            relocation = validated_relocation(case, summary)
            text = initial_text(case, True, relocation, "ab" * 32)
        self.assertEqual(relocation.passing.censored_attempts, ("seed-000-attempt-1",))
        self.assertIn(
            "Relocation changed the PASS witness: manifest seed 1 did not "
            "reproduce here; seed 0 did.",
            text,
        )
        self.assertEqual(text.count("hit the safehermit wall bound (CENSORED)"), 2)

    def test_relocation_requires_historical_hash(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            relocation = validated_relocation(
                case, _relocation_summary(Path(temporary), case)
            )
        with self.assertRaisesRegex(CommentError, "historical record hash"):
            initial_text(case, True, relocation)
        for digest in ("AB" * 32, "ab" * 31, "ab" * 32 + "\n", "zz" * 32):
            with (
                self.subTest(digest=digest),
                self.assertRaisesRegex(CommentError, "not a SHA-256 hex digest"),
            ):
                initial_text(case, True, relocation, digest)

    def test_refuses_relabelled_censored_attempt(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case, censored=1)
            self._edit(
                summary,
                lambda value: value["witness"]["fail"]["decided_attempts"].insert(
                    0, value["witness"]["fail"]["censored_attempts"].pop()
                ),
            )
            with self.assertRaisesRegex(RelocationError, "determinism"):
                validated_relocation(case, summary)

    def test_refuses_omitted_censored_attempt(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case, censored=1)
            self._edit(
                summary,
                lambda value: value["witness"]["pass"]["censored_attempts"].clear(),
            )
            with self.assertRaisesRegex(RelocationError, "omits or invents"):
                validated_relocation(case, summary)

    def test_refuses_decided_bytes_relabelled_as_censored(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case, censored=1)
            censored = root / "seed-011-attempt-1"
            for name in ("stdout.txt", "stderr.txt"):
                decided = (root / "seed-011-attempt-2" / name).read_bytes()
                (censored / name).write_bytes(decided)
            self._rebind(censored)
            with self.assertRaisesRegex(RelocationError, "satisfies the fail oracle"):
                validated_relocation(case, summary)

    def test_refuses_forged_censored_attempt(self) -> None:
        case = _loaded_case()
        forgeries: dict[str, Callable[[Path], object]] = {
            "wrong schema": lambda run: (run / "receipt.json").write_text(
                json.dumps(
                    {"classification": {"oracle": "CENSORED", "returncode": 124}}
                )
            ),
            "not bound to": lambda run: self._edit(
                run / "receipt.json", lambda value: value.update(seed=2)
            ),
            "lacks wall-bound evidence": lambda run: self._edit(
                run / "receipt.json", lambda value: value.update(duration_seconds=3.0)
            ),
            "exit code contradicts": lambda run: (run / "safehermit.txt").write_text(
                "safehermit: unit_result=timeout\nsafehermit: exit_code=134\n"
            ),
        }
        for message, forge in forgeries.items():
            with (
                self.subTest(message=message),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                summary = _relocation_summary(root, case, censored=1)
                run = root / "seed-001-attempt-1"
                forge(run)
                if message == "exit code contradicts":
                    self._rebind(run)
                with self.assertRaisesRegex(RelocationError, message):
                    validated_relocation(case, summary)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case, censored=1)
            run = root / "seed-001-attempt-1"
            (run / "safehermit.txt").write_text("safehermit: exit_code=124\n")
            self._rebind(run)
            with self.assertRaisesRegex(RelocationError, "lacks wall-bound evidence"):
                validated_relocation(case, summary)

    def test_refuses_paths_escaping_through_dotdot_or_symlink(self) -> None:
        case = _loaded_case()
        for flag, escape, message in (
            ("--mount=", "dotdot", "cwd path is not canonical"),
            ("--tmp=", "dotdot", "guest /tmp path is not canonical"),
            ("--mount=", "symlink", "cwd resolves into"),
            ("--sh-report", "symlink", "safehermit report resolves into"),
            ("--verify-log-dir=", "symlink", "verify-log dir resolves into"),
        ):
            with (
                self.subTest(flag=flag, escape=escape),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                forbidden = root / "forbidden"
                forbidden.mkdir()
                (root / "link").symlink_to(forbidden)
                summary = _relocation_summary(root / "runs", case)
                if escape == "dotdot":
                    path = "/home/tfa-relocation-fixture/a/../b/cwd"
                else:
                    path = str(root / "link" / "cwd")

                def edit(value: Any, flag: str = flag, path: str = path) -> None:
                    argv = value["argv"]
                    if flag == "--sh-report":
                        argv[argv.index(flag) + 1] = path
                        return
                    index = next(
                        i
                        for i, token in enumerate(argv)
                        if token.startswith(flag)
                        and (flag != "--mount=" or "/cwd," in token)
                    )
                    argv[index] = (
                        f"--mount=type=bind,source={path},target={path}"
                        if flag == "--mount="
                        else f"{flag}{path}"
                    )

                self._edit(root / "runs" / "seed-001-attempt-1" / "receipt.json", edit)
                with (
                    patch.object(relocation_module, "_FORBIDDEN_ROOT", str(forbidden)),
                    self.assertRaisesRegex(RelocationError, message),
                ):
                    validated_relocation(case, summary)

    def test_refuses_receipt_whose_bytes_contradict_its_oracle(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case)
            run = root / "seed-011-attempt-2"
            stdout = (run / "stdout.txt").read_text()
            (run / "stdout.txt").write_text(stdout.replace("ByteEventType::ACK", ""))
            with self.assertRaisesRegex(RelocationError, "stdout bytes differ"):
                validated_relocation(case, summary)
            self._edit(
                run / "receipt.json",
                lambda value: value["stdout"].update(
                    sha256=hashlib.sha256((run / "stdout.txt").read_bytes()).hexdigest()
                ),
            )
            with self.assertRaisesRegex(RelocationError, "failure-class markers"):
                validated_relocation(case, summary)

    def test_refuses_argv_that_changes_more_than_tmp_paths(self) -> None:
        case = _loaded_case()
        for change in ("--chaos-slowdown-max-factor=2.0", "/tmp/x"):
            with (
                self.subTest(change=change),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                summary = _relocation_summary(root, case)
                run = root / "seed-001-attempt-1"

                def edit(value: dict[str, object], change: str = change) -> None:
                    argv = value["argv"]
                    assert isinstance(argv, list)
                    if change.startswith("/tmp"):
                        argv[argv.index("--sh-report") + 1] = change
                    else:
                        index = next(
                            i
                            for i, token in enumerate(argv)
                            if token.startswith("--chaos-slowdown-max-factor=")
                        )
                        argv[index] = change

                self._edit(run / "receipt.json", edit)
                self._edit(
                    summary,
                    lambda value: edit(value["witness"]["pass"]),
                )
                with self.assertRaisesRegex(RelocationError, "argv"):
                    validated_relocation(case, summary)

    def test_refuses_fresh_stress_seed_and_unreproduced_state(self) -> None:
        case = _loaded_case()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case, case.fresh_stress_seeds[0])
            with self.assertRaisesRegex(RelocationError, "fresh stress seed"):
                validated_relocation(case, summary)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary = _relocation_summary(root, case)
            self._edit(
                summary,
                lambda value: value["witness"]["fail"].update(state="UNCONFIRMED"),
            )
            with self.assertRaisesRegex(RelocationError, "not REPRODUCED"):
                validated_relocation(case, summary)


class ProductionContainmentTest(unittest.TestCase):
    def test_pid_inspection_failure_cannot_preempt_hierarchical_kill(self) -> None:
        cgroup = _ProcessCgroup()
        path = cgroup.path
        process = TrustedSubprocessWithList.Popen(
            executable="/usr/bin/sleep", cmd_args=["30"]
        )
        try:
            cgroup.attach(process.pid)
            with patch.object(cgroup, "_pids", side_effect=ProcessError("unreadable")):
                self.assertEqual(cgroup.terminate_and_remove(), ())
            self.assertEqual(process.wait(timeout=5), -9)
            self.assertFalse(path.exists())
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)


class ProcessTest(unittest.TestCase):
    def setUp(self) -> None:
        helper = Path(__file__).with_name("test_process_migration_helper.py")
        runtime = MagicMock(wrapper=helper, supervisor=helper)
        pinned = patch("testinfra.tfa.process._pinned_runtime", return_value=runtime)
        pinned.start()
        self.addCleanup(pinned.stop)
        entrypoint = patch(
            "testinfra.tfa.process._codex_entrypoint_identity",
            return_value={
                "owner_uid": 0,
                "path": "/usr/local/bin/codex",
                "resolved_path": "/usr/local/bin/codex_cli/codex",
                "target": "/usr/local/bin/codex_cli/codex",
            },
        )
        entrypoint.start()
        self.addCleanup(entrypoint.stop)

    def test_current_verify_translation_preserves_historical_strictness(self) -> None:
        historical = hermit_attempt_argv(
            tuple(_argv(1)),
            Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
            Path("/tmp/tfa-phase2/controller"),
            180,
            Path("/tmp/tfa-phase2/safehermit"),
        )
        translated = hermit_module._current_verify_argv(historical)
        boundary = translated.index("--")
        self.assertEqual(translated[:boundary].count("--verify"), 1)
        self.assertEqual(translated[:boundary].count("--verify-verbose"), 1)
        self.assertNotIn("--verify-strict", translated[:boundary])
        self.assertNotIn("--keep-logs", translated[:boundary])
        self.assertFalse(
            any(token.startswith("--timeout=") for token in translated[:boundary])
        )
        self.assertEqual(
            hermit_module._safehermit_deadline(translated),
            hermit_module._safehermit_deadline(historical)
            - hermit_module._HERMIT_UNWIND_SECONDS,
        )
        self.assertFalse(
            any(
                token.startswith("--verify-log-dir=") for token in translated[:boundary]
            )
        )
        self.assertEqual(translated[boundary:], historical[historical.index("--") :])

    def test_current_verify_translation_rejects_a_lowered_contract(self) -> None:
        historical = list(
            hermit_attempt_argv(
                tuple(_argv(1)),
                Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                Path("/tmp/tfa-phase2/controller"),
                180,
                Path("/tmp/tfa-phase2/safehermit"),
            )
        )
        historical.remove("--verify-strict")
        with self.assertRaisesRegex(HermitError, "strict verification contract"):
            hermit_module._current_verify_argv(tuple(historical))

    def test_validation_argv_rebinds_patched_checkout_buck_out(self) -> None:
        case = _loaded_case()
        elf = Path("/tmp/tfa-phase2/cases/asyncssl/buck-out/v2/new/AsyncSSLSocketTest")
        root = Path("/tmp/validation")
        argv = hermit_attempt_argv(
            pipeline_module._validation_base_argv(case, 9, elf, root),
            Path(case.working_directory),
            root / "controller-cwd",
            180,
            safehermit_wrapper_path(root),
        )
        self.assertIn(
            "--mount=type=bind,source=/tmp/tfa-phase2/cases/asyncssl/buck-out,"
            "target=/tmp/tfa-phase2/cases/asyncssl/buck-out",
            argv,
        )
        self.assertEqual(argv[argv.index("--") + 1], str(elf))

    def test_packaged_hermit_replaces_only_qualified_binary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "hermit-test-bin"
            binary.write_bytes(b"qualified imported Hermit\n")
            binary.chmod(0o500)
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            compatibility = root / "hermit-import-compatibility.v1.json"
            value = json.loads(
                Path(manifest_module.__file__)
                .with_name("hermit-import-compatibility.v1.json")
                .read_text()
            )
            value["historical_witness"]["revision"] = PACKAGED_HERMIT_REVISION
            value["historical_witness"]["sha256"] = digest
            value["qualification_probe"]["sha256"] = digest
            value["qualification_probe"]["state"] = "QUALIFIED"
            value["qualification_probe"]["blockers"] = []
            value["packaged_executable"]["sha256"] = digest
            value["packaged_executable"]["state"] = "QUALIFIED"
            value["packaged_executable"]["observed_blockers"] = []
            compatibility.write_text(json.dumps(value))
            original = tuple(_argv(1))
            with (
                patch(
                    "testinfra.tfa.hermit._packaged_resource",
                    side_effect=lambda name: (
                        compatibility
                        if name == "hermit-import-compatibility.v1.json"
                        else binary
                    ),
                ),
                patch(
                    "testinfra.tfa.hermit.PACKAGED_HERMIT_QUALIFICATION_SHA256",
                    digest,
                ),
                patch("testinfra.tfa.hermit.PACKAGED_HERMIT_RESOURCE_SHA256", digest),
                patch(
                    "testinfra.tfa.hermit._qualification_receipt_record",
                    return_value={"sha256": "test"},
                ),
            ):
                rewritten = packaged_hermit_argv(
                    original,
                    PACKAGED_HERMIT_REVISION,
                    digest,
                    root / "pinned" / "hermit",
                )
            expected_index = original.index(
                "/home/newton/work/dev-hermit/ignored/"
                "tfa-rlimit-core-rerun-20260920-4630ee2a/"
                "stable-5c422b85/hermit-4630ee2a"
            )
            self.assertEqual(rewritten[expected_index], str(root / "pinned" / "hermit"))
            self.assertEqual(
                stat.S_IMODE((root / "pinned" / "hermit").stat().st_mode), 0o500
            )
            self.assertEqual(
                hashlib.sha256((root / "pinned" / "hermit").read_bytes()).hexdigest(),
                digest,
            )
            self.assertEqual(
                rewritten[:expected_index] + rewritten[expected_index + 1 :],
                original[:expected_index] + original[expected_index + 1 :],
            )

    def test_packaged_hermit_rejects_unqualified_revision_or_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "hermit-test-bin"
            binary.write_bytes(b"qualified imported Hermit\n")
            binary.chmod(0o500)
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            compatibility = root / "hermit-import-compatibility.v1.json"
            value = json.loads(
                Path(manifest_module.__file__)
                .with_name("hermit-import-compatibility.v1.json")
                .read_text()
            )
            value["historical_witness"]["revision"] = PACKAGED_HERMIT_REVISION
            value["historical_witness"]["sha256"] = digest
            value["qualification_probe"]["sha256"] = digest
            value["qualification_probe"]["state"] = "QUALIFIED"
            value["qualification_probe"]["blockers"] = []
            value["packaged_executable"]["sha256"] = digest
            value["packaged_executable"]["state"] = "QUALIFIED"
            value["packaged_executable"]["observed_blockers"] = []
            compatibility.write_text(json.dumps(value))
            for revision, expected_hash in (
                ("different", digest),
                (PACKAGED_HERMIT_REVISION, "0" * 64),
            ):
                with (
                    self.subTest(revision=revision, expected_hash=expected_hash),
                    patch(
                        "testinfra.tfa.hermit._packaged_resource",
                        side_effect=lambda name: (
                            compatibility
                            if name == "hermit-import-compatibility.v1.json"
                            else binary
                        ),
                    ),
                    patch(
                        "testinfra.tfa.hermit.PACKAGED_HERMIT_QUALIFICATION_SHA256",
                        digest,
                    ),
                    patch(
                        "testinfra.tfa.hermit.PACKAGED_HERMIT_RESOURCE_SHA256", digest
                    ),
                    patch(
                        "testinfra.tfa.hermit._qualification_receipt_record",
                        return_value={"sha256": "test"},
                    ),
                    self.assertRaisesRegex(
                        HermitError, "identity differs from the qualified witness"
                    ),
                ):
                    packaged_hermit_argv(
                        tuple(_argv(1)),
                        revision,
                        expected_hash,
                        root / "pinned" / "hermit",
                    )
                identity = json.loads((root / "pinned/identity.json").read_text())
                self.assertEqual(identity["state"], "WITNESS_MISMATCH")

    def test_packaged_hermit_records_exact_blocked_compatibility(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "hermit-test-bin"
            binary.write_bytes(b"current imported Hermit\n")
            binary.chmod(0o500)
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            compatibility = root / "hermit-import-compatibility.v1.json"
            value = json.loads(
                Path(manifest_module.__file__)
                .with_name("hermit-import-compatibility.v1.json")
                .read_text()
            )
            value["packaged_executable"]["sha256"] = digest
            value["qualification_probe"]["sha256"] = digest
            compatibility.write_text(json.dumps(value))
            receipts = (
                Path(manifest_module.__file__)
                .with_name("hermit-import-receipts.v1.json")
                .resolve(strict=True)
            )

            def resource(name: str) -> Path:
                if name == "hermit-import-compatibility.v1.json":
                    return compatibility
                if name == "hermit-import-receipts.v1.json":
                    return receipts
                return binary

            with (
                patch(
                    "testinfra.tfa.hermit._packaged_resource",
                    side_effect=resource,
                ),
                patch(
                    "testinfra.tfa.hermit.PACKAGED_HERMIT_RESOURCE_SHA256",
                    digest,
                ),
                patch(
                    "testinfra.tfa.hermit.PACKAGED_HERMIT_QUALIFICATION_SHA256",
                    digest,
                ),
                patch(
                    "testinfra.tfa.hermit._qualification_receipt_record",
                    return_value={"sha256": "test"},
                ),
                self.assertRaisesRegex(
                    HermitError,
                    "packaged Hermit compatibility is BLOCKED.*"
                    "sanitizer guests cannot initialize RLIMIT_CORE",
                ),
            ):
                packaged_hermit_argv(
                    tuple(_argv(1)),
                    str(value["historical_witness"]["revision"]),
                    str(value["historical_witness"]["sha256"]),
                    root / "pinned" / "hermit",
                )
            identity = json.loads((root / "pinned/identity.json").read_text())
            self.assertEqual(identity["schema"], "tfa.packaged-hermit-compatibility.v1")
            self.assertEqual(identity["state"], "BLOCKED")
            self.assertEqual(identity["packaged_executable"]["sha256"], digest)
            self.assertEqual(
                identity["expected_witness"],
                {
                    "revision": value["historical_witness"]["revision"],
                    "sha256": value["historical_witness"]["sha256"],
                },
            )

    def _assert_real_packaged_hermit_refusal(
        self, state: str, packaged_sha256: str
    ) -> None:
        # Both refusal states write the full identity record and pin nothing.
        case = _loaded_case()
        value = json.loads(
            Path(manifest_module.__file__)
            .with_name("hermit-import-compatibility.v1.json")
            .read_text()
        )
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "runtime" / "hermit"
            with self.assertRaisesRegex(
                HermitError,
                re.escape(
                    f"packaged Hermit compatibility is {state}: "
                    f"expected revision={case.hermit_revision} "
                    f"sha256={case.hermit_sha256}; "
                    f"target={value['target']} "
                    f"import_commit={value['fbsource_import_commit']} "
                    f"packaged revision={PACKAGED_HERMIT_REVISION} "
                    f"sha256={packaged_sha256}; "
                ),
            ):
                packaged_hermit_argv(
                    case.pass_argv,
                    case.hermit_revision,
                    case.hermit_sha256,
                    destination,
                )
            self.assertFalse(os.path.lexists(destination))
            identity = json.loads((destination.parent / "identity.json").read_text())
            self.assertEqual(identity["schema"], "tfa.packaged-hermit-compatibility.v1")
            self.assertEqual(identity["state"], state)
            self.assertEqual(
                identity["expected_witness"],
                {"revision": case.hermit_revision, "sha256": case.hermit_sha256},
            )
            self.assertEqual(identity["target"], value["target"])
            self.assertEqual(
                identity["fbsource_import_commit"], value["fbsource_import_commit"]
            )
            self.assertEqual(identity["packaged_revision"], PACKAGED_HERMIT_REVISION)
            self.assertEqual(identity["packaged_executable"]["sha256"], packaged_sha256)
            self.assertEqual(
                identity["packaged_executable"]["state"],
                value["packaged_executable"]["state"],
            )
            self.assertEqual(
                identity["packaged_executable"]["configured_target"],
                value["packaged_executable"]["configured_target"],
            )
            self.assertEqual(
                identity["packaged_blockers"],
                value["packaged_executable"]["observed_blockers"],
            )
            self.assertEqual(
                identity["blockers"], value["qualification_probe"]["blockers"]
            )
            self.assertEqual(
                identity["qualification"],
                {
                    "configured_target": value["qualification_probe"][
                        "configured_target"
                    ],
                    "mode": value["qualification_probe"]["mode"],
                    "sha256": value["qualification_probe"]["sha256"],
                    "state": value["qualification_probe"]["state"],
                },
            )
            self.assertEqual(
                identity["qualification_receipts"]["sha256"],
                hermit_module.PACKAGED_HERMIT_RECEIPTS_SHA256,
            )

    def test_real_packaged_hermit_is_bound_to_blocked_provenance(self) -> None:
        value = json.loads(
            Path(manifest_module.__file__)
            .with_name("hermit-import-compatibility.v1.json")
            .read_text()
        )
        self.assertEqual(
            value["packaged_executable"]["sha256"], PACKAGED_HERMIT_RESOURCE_SHA256
        )
        self.assertEqual(value["packaged_executable"]["state"], "BLOCKED")
        # The packaged resource may be a byte-identical build or a rebuild whose
        # fb_build_info stamps differ. The observed bytes decide the one state
        # the gate must record: an exact provenance match is refused as BLOCKED
        # by the reviewed packaged state, and any other digest as
        # PROVENANCE_MISMATCH before a compatibility decision is made.
        resource = hermit_module._packaged_resource(
            hermit_module._PACKAGED_HERMIT_RESOURCE
        )
        observed = hashlib.sha256(resource.read_bytes()).hexdigest()
        required = (
            "BLOCKED"
            if observed == PACKAGED_HERMIT_RESOURCE_SHA256
            else "PROVENANCE_MISMATCH"
        )
        self._assert_real_packaged_hermit_refusal(required, observed)

    def test_packaged_hermit_mutated_bytes_are_provenance_mismatch(self) -> None:
        # Substitute only the packaged executable; the reviewed compatibility
        # record, receipts and pinned digests stay real, so the mismatch path
        # is exercised regardless of how the ambient resource was built.
        original_resource = hermit_module._packaged_resource
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "hermit-test-bin"
            binary.write_bytes(b"mutated packaged Hermit\n")
            binary.chmod(0o500)
            mutated = hashlib.sha256(binary.read_bytes()).hexdigest()
            self.assertNotEqual(mutated, PACKAGED_HERMIT_RESOURCE_SHA256)

            def resource(name: str) -> Path:
                if name == hermit_module._PACKAGED_HERMIT_RESOURCE:
                    return binary
                return original_resource(name)

            with patch("testinfra.tfa.hermit._packaged_resource", side_effect=resource):
                self._assert_real_packaged_hermit_refusal(
                    "PROVENANCE_MISMATCH", mutated
                )

    def test_safehermit_resources_are_resolved_only_when_pinned(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            safehermit = root / "source-safehermit"
            bounded = root / "source-bounded-run-space"
            safehermit.write_bytes(b"#!/bin/sh\nexit 0\n")
            bounded.write_bytes(b"#!/bin/sh\nexit 0\n")
            safehermit.chmod(0o700)
            bounded.chmod(0o700)
            safe_digest = hashlib.sha256(safehermit.read_bytes()).hexdigest()
            bounded_digest = hashlib.sha256(bounded.read_bytes()).hexdigest()
            attempt = root / "attempt"
            resolved: list[str] = []

            def resource(name: str) -> Path:
                resolved.append(name)
                return safehermit if name == "safehermit" else bounded

            with (
                patch("testinfra.tfa.hermit._packaged_resource", side_effect=resource),
                patch("testinfra.tfa.hermit.SAFEHERMIT_SHA256", safe_digest),
                patch("testinfra.tfa.hermit.BOUNDED_RUN_SPACE_SHA256", bounded_digest),
            ):
                records = _pin_safehermit_runtime(attempt)
            self.assertEqual(resolved, ["safehermit", "bounded-run-space"])
            for path in safehermit_runtime_paths(attempt):
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o500)
            safe_record = records["safehermit"]
            bounded_record = records["bounded_run_space"]
            self.assertIsInstance(safe_record, dict)
            self.assertIsInstance(bounded_record, dict)
            self.assertEqual(safe_record["sha256"], safe_digest)
            self.assertEqual(bounded_record["sha256"], bounded_digest)

    def test_codex_mount_isolation_preflight_fails_before_agent_launch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            checkout.mkdir()
            output = root / "agent-output"
            output.mkdir()
            process_dir = root / "control" / "diagnostic-agent"
            process_dir.mkdir(parents=True)
            schema = root / "schema.json"
            schema.write_text("{}\n")
            arguments = codex_arguments(
                "must not run",
                checkout,
                schema,
                process_dir / "final-message.json",
                None,
            )
            with (
                patch(
                    "testinfra.tfa.process._codex_environment_preflight",
                    side_effect=ProcessError(
                        "Codex native workspace sandbox preflight failed "
                        "(tfa.codex-environment-preflight.v3; codex-cli 0.156.0; "
                        "state=blocked): owner-managed Codex lacks daemon-socket fixes"
                    ),
                ) as preflight,
                patch("testinfra.tfa.process._spawn_contained") as spawn,
                self.assertRaisesRegex(
                    ProcessError, "owner-managed Codex lacks daemon-socket fixes"
                ),
            ):
                run_process(
                    "/usr/local/bin/codex",
                    arguments,
                    checkout,
                    process_dir,
                    10,
                )
            preflight.assert_called_once()
            spawn.assert_not_called()

    def test_codex_preflight_classifies_known_mount_isolation_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = MagicMock(
                returncode=0,
                stdout="codex-cli 0.156.0\n",
                stderr="",
            )
            manifest = {
                "kind": "file",
                "link_count": 1,
                "mode": 0o755,
                "owner_uid": 0,
                "path": "/usr/local/bin/codex_cli/codex.real",
                "sha256": (
                    "66494455a90b6f45f1fb93c73847d610337a12252a8f10a8a393f20ca213ae72"
                ),
                "size": 2245,
            }
            with (
                patch.object(TrustedSubprocessWithList, "run", return_value=version),
                patch(
                    "testinfra.tfa.process._root_owned_regular_record",
                    return_value={
                        "kind": "file",
                        "link_count": 1,
                        "mode": 0o755,
                        "owner_uid": 0,
                        "path": "/usr/local/bin/codex_cli/codex",
                        "sha256": (
                            "71b8f5bde99ce8b4cff6ce7c7cbe79a0fe8d6bd36a4427e5"
                            "d308bda4d065ebf0"
                        ),
                        "size": 1,
                    },
                ),
                patch(
                    "testinfra.tfa.process._codex_release_identity",
                    return_value=("stable", manifest),
                ),
                self.assertRaisesRegex(
                    ProcessError, "owner-managed Codex lacks daemon-socket fixes"
                ),
            ):
                process_module._codex_environment_preflight(
                    root, root / "preflight", {}
                )
            identity = json.loads((root / "preflight/identity.json").read_text())
            self.assertEqual(identity["schema"], "tfa.codex-environment-preflight.v3")
            self.assertEqual(identity["version"], "codex-cli 0.156.0")
            self.assertEqual(identity["state"], "blocked")
            self.assertEqual(identity["release_channel"], "stable")
            self.assertEqual(identity["release_manifest"], manifest)
            self.assertEqual(
                identity["blocker"],
                process_module._CODEX_SOCKET_ISOLATION_BLOCKER,
            )
            _assert_socket_isolation_blocker_text(self, identity["blocker"])
            self.assertIsNone(identity["payload_sha256"])

    def test_codex_preflight_records_version_command_startup_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with (
                patch.object(
                    TrustedSubprocessWithList,
                    "run",
                    side_effect=OSError("local process unavailable"),
                ),
                patch(
                    "testinfra.tfa.process._root_owned_regular_record",
                    return_value={
                        "kind": "file",
                        "link_count": 1,
                        "mode": 0o755,
                        "owner_uid": 0,
                        "path": "/usr/local/bin/codex_cli/codex",
                        "sha256": "0" * 64,
                        "size": 1,
                    },
                ),
                patch(
                    "testinfra.tfa.process._codex_release_identity",
                    return_value=(
                        "stable",
                        {
                            "path": "/usr/local/bin/codex_cli/codex.real",
                            "sha256": "1" * 64,
                        },
                    ),
                ),
                self.assertRaisesRegex(
                    ProcessError,
                    "tfa.codex-environment-preflight.v3.*"
                    "version command could not start",
                ),
            ):
                process_module._codex_environment_preflight(
                    root, root / "preflight", {}
                )
            identity = json.loads((root / "preflight/identity.json").read_text())
            self.assertEqual(identity["state"], "startup-failed")
            self.assertIsNone(identity["version"])
            self.assertEqual(identity["schema"], "tfa.codex-environment-preflight.v3")

    def test_codex_preflight_classifies_owner_managed_latest_as_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = MagicMock(
                returncode=0,
                stdout="codex-cli 0.156.1\n",
                stderr=process_module._CODEX_VERSION_STDERR["latest"][-1],
            )
            manifest = {
                "path": "/usr/local/bin/codex_cli/codex_latest.real",
                "sha256": (
                    "4b776a3e1d72706ab648db6fb24f5baa1a1ceaec2d4e3f07bd408db4dc0ecab5"
                ),
            }
            with (
                patch.object(TrustedSubprocessWithList, "run", return_value=version),
                patch(
                    "testinfra.tfa.process._root_owned_regular_record",
                    return_value={
                        "kind": "file",
                        "link_count": 1,
                        "mode": 0o755,
                        "owner_uid": 0,
                        "path": "/usr/local/bin/codex_cli/codex",
                        "sha256": (
                            "71b8f5bde99ce8b4cff6ce7c7cbe79a0fe8d6bd36a4427e5"
                            "d308bda4d065ebf0"
                        ),
                        "size": 1,
                    },
                ),
                patch(
                    "testinfra.tfa.process._codex_release_identity",
                    return_value=("latest", manifest),
                ),
                self.assertRaisesRegex(
                    ProcessError, "owner-managed Codex lacks daemon-socket fixes"
                ),
            ):
                process_module._codex_environment_preflight(
                    root,
                    root / "preflight",
                    {"META_CODEX_RELEASE": "latest"},
                )
            identity = json.loads((root / "preflight/identity.json").read_text())
            self.assertEqual(identity["state"], "blocked")
            self.assertEqual(identity["release_channel"], "latest")
            self.assertEqual(identity["release_manifest"], manifest)
            self.assertEqual(
                identity["blocker"], process_module._CODEX_SOCKET_ISOLATION_BLOCKER
            )
            _assert_socket_isolation_blocker_text(self, identity["blocker"])
            self.assertIsNone(identity["payload_sha256"])

    def test_codex_preflight_rejects_release_manifest_race(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = MagicMock(
                returncode=0,
                stdout="codex-cli 0.156.0\n",
                stderr="",
            )
            before = {
                "path": "/usr/local/bin/codex_cli/codex.real",
                "sha256": "1" * 64,
            }
            after = {**before, "sha256": "2" * 64}
            with (
                patch.object(TrustedSubprocessWithList, "run", return_value=version),
                patch(
                    "testinfra.tfa.process._root_owned_regular_record",
                    return_value={
                        "kind": "file",
                        "link_count": 1,
                        "mode": 0o755,
                        "owner_uid": 0,
                        "path": "/usr/local/bin/codex_cli/codex",
                        "sha256": "0" * 64,
                        "size": 1,
                    },
                ),
                patch(
                    "testinfra.tfa.process._codex_release_identity",
                    side_effect=(("stable", before), ("stable", after)),
                ),
                self.assertRaisesRegex(ProcessError, "release manifest changed"),
            ):
                process_module._codex_environment_preflight(
                    root, root / "preflight", {}
                )
            identity = json.loads((root / "preflight/identity.json").read_text())
            self.assertEqual(identity["state"], "identity-changed")
            self.assertEqual(identity["release_manifest"], before)
            self.assertEqual(identity["release_manifest_after"], after)

    def test_codex_preflight_rejects_entrypoint_retarget(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            before = {
                "owner_uid": 0,
                "path": "/usr/local/bin/codex",
                "resolved_path": "/usr/local/bin/codex_cli/codex",
                "target": "codex_cli/codex",
            }
            after = {**before, "target": "codex_cli/replacement"}
            version = MagicMock(returncode=0, stdout="codex-cli 9.9.9\n", stderr="")
            executable = {
                "kind": "file",
                "mode": 0o755,
                "path": "/usr/local/bin/codex_cli/codex",
                "sha256": "0" * 64,
                "size": 1,
            }
            manifest = {
                "path": "/usr/local/bin/codex_cli/codex.real",
                "sha256": "1" * 64,
            }
            with (
                patch.object(TrustedSubprocessWithList, "run", return_value=version),
                patch(
                    "testinfra.tfa.process._codex_entrypoint_identity",
                    side_effect=(before, after),
                ),
                patch(
                    "testinfra.tfa.process._root_owned_regular_record",
                    return_value=executable,
                ),
                patch(
                    "testinfra.tfa.process._codex_release_identity",
                    return_value=("stable", manifest),
                ),
                self.assertRaisesRegex(ProcessError, "release manifest changed"),
            ):
                process_module._codex_environment_preflight(
                    root, root / "preflight", {}
                )
            identity = json.loads((root / "preflight/identity.json").read_text())
            self.assertEqual(identity["state"], "identity-changed")
            self.assertEqual(identity["entrypoint"], before)
            self.assertEqual(identity["entrypoint_after"], after)

    def test_codex_spawn_rejects_post_preflight_channel_drift(self) -> None:
        entrypoint = {
            "owner_uid": 0,
            "path": "/usr/local/bin/codex",
            "resolved_path": "/usr/local/bin/codex_cli/codex",
            "target": "codex_cli/codex",
        }
        executable = {
            "path": "/usr/local/bin/codex_cli/codex",
            "sha256": "0" * 64,
        }
        stable_manifest = {
            "path": "/usr/local/bin/codex_cli/codex.real",
            "sha256": "1" * 64,
        }
        latest_manifest = {
            "path": "/usr/local/bin/codex_cli/codex_latest.real",
            "sha256": "2" * 64,
        }
        expected = {
            "entrypoint": entrypoint,
            "release_channel": "stable",
            "release_manifest": stable_manifest,
            "resolved_executable": executable,
        }
        with (
            patch(
                "testinfra.tfa.process._codex_entrypoint_identity",
                return_value=entrypoint,
            ),
            patch(
                "testinfra.tfa.process._codex_release_identity",
                return_value=("latest", latest_manifest),
            ),
            patch(
                "testinfra.tfa.process._root_owned_regular_record",
                return_value=executable,
            ),
            self.assertRaisesRegex(ProcessError, "identity changed after preflight"),
        ):
            process_module._validated_codex_launch_record(expected, {})

    def test_codex_preflight_rejects_unreviewed_release_channel(self) -> None:
        with self.assertRaisesRegex(ProcessError, "release channel is not reviewed"):
            process_module._codex_release_identity({"META_CODEX_RELEASE": "0.157.0"})

    def test_codex_preflight_rejects_unreviewed_executable_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = MagicMock(
                returncode=0,
                stdout="codex-cli 9.9.9\n",
                stderr=process_module._CODEX_VERSION_STDERR["stable"][1],
            )
            executable = {
                "kind": "file",
                "link_count": 1,
                "mode": 0o755,
                "owner_uid": 0,
                "path": "/usr/local/bin/codex_cli/codex",
                "sha256": "0" * 64,
                "size": 1,
            }
            manifest = {
                "path": "/usr/local/bin/codex_cli/codex.real",
                "sha256": "1" * 64,
            }
            with (
                patch.object(TrustedSubprocessWithList, "run", return_value=version),
                patch(
                    "testinfra.tfa.process._root_owned_regular_record",
                    return_value=executable,
                ),
                patch(
                    "testinfra.tfa.process._codex_release_identity",
                    return_value=("stable", manifest),
                ),
                self.assertRaisesRegex(
                    ProcessError,
                    "state=unsupported.*no reviewed workspace-sandbox compatibility",
                ),
            ):
                process_module._codex_environment_preflight(
                    root, root / "preflight", {}
                )
            identity = json.loads((root / "preflight/identity.json").read_text())
            self.assertEqual(identity["state"], "unsupported")
            self.assertEqual(identity["version"], "codex-cli 9.9.9")
            self.assertEqual(
                identity["version_stderr"],
                process_module._CODEX_VERSION_STDERR["stable"][1],
            )
            self.assertEqual(identity["resolved_executable"], executable)
            self.assertEqual(identity["release_manifest"], manifest)

    def test_cli_timeout_type_rejects_nonfinite_and_nonpositive_values(self) -> None:
        @click.command()
        @click.option("--timeout-seconds", type=positive_finite_timeout, required=True)
        def command(timeout_seconds: float) -> None:
            click.echo(timeout_seconds)

        runner = CliRunner()
        for value in ("nan", "inf", "-inf", "0", "-1"):
            with self.subTest(value=value):
                result = runner.invoke(command, ["--timeout-seconds", value])
                self.assertEqual(result.exit_code, 2, result.output)
                self.assertIn("Invalid value for '--timeout-seconds'", result.output)
        result = runner.invoke(command, ["--timeout-seconds", "0.5"])
        self.assertEqual(result.exit_code, 0, result.output)
        self.assertEqual(result.output, "0.5\n")

    def test_process_rejects_unbounded_timeout_inputs_before_launch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for value in (float("nan"), float("inf"), float("-inf"), 0.0, -1.0):
                artifact = root / repr(value)
                with (
                    self.subTest(value=value),
                    self.assertRaisesRegex(
                        ProcessError, "execution timeout must be positive and finite"
                    ),
                ):
                    run_process(
                        "/usr/local/bin/fbpython",
                        ("-c", "raise SystemExit(0)"),
                        root,
                        artifact,
                        value,
                    )
                self.assertFalse(artifact.exists())

    def test_report_publisher_never_follows_links(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_parent = root / "control" / "diagnostic-agent"
            source_parent.mkdir(parents=True)
            source = source_parent / "final-message.json"
            source.write_text('{"safe":true}\n')
            output = root / "agent-output"
            output.mkdir()
            destination = output / "report.json"
            target = root / "control-target"
            target.write_text("sentinel\n")

            source.unlink()
            os.link(target, source)
            with self.assertRaises(ProcessError):
                publish_codex_report(source, destination)
            self.assertEqual(target.read_text(), "sentinel\n")
            source.unlink()
            source.write_text('{"safe":true}\n')

            destination.symlink_to(target)
            with self.assertRaises(ProcessError):
                publish_codex_report(source, destination)
            self.assertEqual(target.read_text(), "sentinel\n")

            destination.unlink()
            os.link(target, destination)
            with self.assertRaises(ProcessError):
                publish_codex_report(source, destination)
            self.assertEqual(target.read_text(), "sentinel\n")

            destination.unlink()
            destination.write_text("untrusted\n")
            publish_codex_report(source, destination)
            self.assertEqual(destination.read_text(), '{"safe":true}\n')
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o600)

    def test_report_publisher_requires_one_absolute_case_layout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first = root / "first"
            source_parent = first / "control" / "diagnostic-agent"
            source_parent.mkdir(parents=True)
            source = source_parent / "final-message.json"
            source.write_text('{"safe":true}\n')
            output = first / "agent-output"
            output.mkdir()
            destination = output / "report.json"
            second_output = root / "second" / "agent-output"
            second_output.mkdir(parents=True)
            unrelated_output = root / "unrelated" / "agent-output"
            unrelated_output.mkdir(parents=True)
            invalid = {
                "cross-case": (source, second_output / "report.json"),
                "relative-source": (
                    Path("first/control/diagnostic-agent/final-message.json"),
                    destination,
                ),
                "relative-destination": (
                    source,
                    Path("first/agent-output/report.json"),
                ),
                "traversal": (
                    first
                    / "control"
                    / ".."
                    / "control"
                    / "diagnostic-agent"
                    / "final-message.json",
                    destination,
                ),
                "unrelated-output": (source, unrelated_output / "report.json"),
                "wrong-source-name": (
                    source_parent / "other.json",
                    destination,
                ),
            }
            for name, (candidate_source, candidate_destination) in invalid.items():
                with self.subTest(name=name), self.assertRaises(ProcessError):
                    publish_codex_report(candidate_source, candidate_destination)
            self.assertFalse(destination.exists())

    def test_codex_trusted_outputs_reject_hard_links_before_launch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for mode in ("fresh", "resume", "reviewer"):
                with self.subTest(mode=mode):
                    case = root / mode
                    checkout = case / "checkout"
                    checkout.mkdir(parents=True)
                    schema = case / "schema.json"
                    schema.write_text("{}\n")
                    sentinel = case / "sentinel"
                    sentinel.write_text("sentinel\n")
                    if mode == "reviewer":
                        reviewer = case / "reviewer-output"
                        workspace = reviewer / "workspace"
                        workspace.mkdir(parents=True)
                        report = reviewer / "review.json"
                        arguments = codex_review_arguments(
                            "prompt", workspace, schema, report
                        )
                        process_cwd = workspace
                    else:
                        (case / "agent-output").mkdir()
                        process_dir = case / "control" / "diagnostic-agent"
                        process_dir.mkdir(parents=True)
                        report = process_dir / "final-message.json"
                        arguments = codex_arguments(
                            "prompt",
                            checkout,
                            schema,
                            report,
                            "session-1" if mode == "resume" else None,
                        )
                        process_cwd = checkout
                    os.link(sentinel, report)
                    artifact = case / "must-not-launch"
                    with self.assertRaisesRegex(ProcessError, "report path is unsafe"):
                        run_process(
                            "/usr/local/bin/codex",
                            arguments,
                            process_cwd,
                            artifact,
                            10,
                        )
                    self.assertEqual(sentinel.read_text(), "sentinel\n")
                    self.assertFalse(artifact.exists())

    def test_process_rejects_dangerous_codex_sandbox_bypass_before_launch(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            artifact = Path(temporary) / "run"
            with self.assertRaisesRegex(
                ProcessError, "Codex argv does not match a supported contract"
            ):
                run_process(
                    "/usr/local/bin/codex",
                    ("exec", "--dangerously-bypass-approvals-and-sandbox", "prompt"),
                    Path(temporary),
                    artifact,
                    10,
                )
            self.assertFalse(artifact.exists())

    def test_process_rejects_spoofed_codex_contracts_before_launch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            checkout.mkdir()
            output = root / "agent-output"
            output.mkdir()
            control = root / "control"
            control.mkdir()
            agent_process = control / "diagnostic-agent"
            agent_process.mkdir()
            schema = control / "schema.json"
            schema.write_text("{}\n")
            final_message = agent_process / "final-message.json"
            safe = codex_arguments("prompt", checkout, schema, final_message, None)
            delimiter = safe.index("--")
            spoof = str(root / "outside")
            extras = {
                "network-duplicate": (
                    "-c",
                    "sandbox_workspace_write.network_access=true",
                ),
                "approval-duplicate": ("-c", 'approval_policy="on-request"'),
                "long-config": (
                    "--config",
                    "sandbox_workspace_write.network_access=true",
                ),
                "attached-config": ("-csandbox_workspace_write.network_access=true",),
                "sandbox-duplicate": ("--sandbox", "danger-full-access"),
                "short-sandbox": ("-s", "danger-full-access"),
                "sandbox-equals": ("--sandbox=danger-full-access",),
                "short-approval": ("-a", "never"),
                "approval-equals": ("--ask-for-approval=never",),
                "approve-for-me": ("--approve-for-me",),
                "bypass": ("--dangerously-bypass-approvals-and-sandbox",),
                "add-dir": ("--add-dir", spoof),
                "short-cwd": ("-C", spoof),
                "long-cwd": (f"--cd={spoof}",),
                "short-output": ("-o", f"{spoof}/report"),
                "long-output": (f"--output-last-message={spoof}/report",),
                "schema": ("--output-schema", f"{spoof}/schema"),
                "worktree": ("--worktree",),
                "remote": ("--remote", "ws://127.0.0.1:1"),
                "remote-auth": ("--remote-auth-token-env", "TOKEN"),
                "oss": ("--oss",),
                "provider": ("--local-provider", "ollama"),
                "feature": ("--enable", "unsafe"),
                "proxy": ("--with-proxy", "http://127.0.0.1:1"),
                "mcp": ("--add-mcp-tools",),
                "meta-feature": ("--meta-force-enable-features=unsafe",),
                "disable": ("--disable", "shell_snapshot"),
            }
            conflicts = {
                name: (*safe[:delimiter], *extra, *safe[delimiter:])
                for name, extra in extras.items()
            }
            for name, option in (
                ("dangling-sandbox", "--sandbox"),
                ("dangling-config", "-c"),
            ):
                conflicts[name] = (*safe[:delimiter], option)
            reordered = list(safe)
            json_index = reordered.index("--json")
            reordered[json_index], reordered[json_index + 1] = (
                reordered[json_index + 1],
                reordered[json_index],
            )
            conflicts["ordering"] = tuple(reordered)
            add_dir_spoof = list(safe)
            add_dir_spoof[add_dir_spoof.index("--add-dir") + 1] = spoof
            conflicts["add-dir-path"] = tuple(add_dir_spoof)
            workspace_spoof = list(safe)
            workspace_spoof[workspace_spoof.index("-C") + 1] = spoof
            conflicts["workspace-path"] = tuple(workspace_spoof)
            for name, arguments in conflicts.items():
                artifact = root / name
                with self.subTest(name=name):
                    with self.assertRaises(ProcessError):
                        run_process(
                            "/usr/local/bin/codex",
                            arguments,
                            checkout,
                            artifact,
                            10,
                        )
                    self.assertFalse(artifact.exists())

            symlink_parent_root = root / "symlink-parent"
            symlink_parent_checkout = symlink_parent_root / "checkout"
            symlink_parent_checkout.mkdir(parents=True)
            symlink_parent_process = (
                symlink_parent_root / "control" / "diagnostic-agent"
            )
            symlink_parent_process.mkdir(parents=True)
            symlink_target = symlink_parent_root / "elsewhere"
            symlink_target.mkdir()
            (symlink_parent_root / "agent-output").symlink_to(
                symlink_target, target_is_directory=True
            )

            control_parent_root = root / "control-parent"
            control_parent_checkout = control_parent_root / "checkout"
            control_parent_checkout.mkdir(parents=True)
            control_parent_process = (
                control_parent_root / "control" / "diagnostic-agent"
            )
            control_parent_process.mkdir(parents=True)
            control_target = control_parent_root / "control-target"
            control_target.mkdir()
            (control_parent_root / "agent-output").symlink_to(
                control_target, target_is_directory=True
            )

            report_link_root = root / "report-link"
            report_link_checkout = report_link_root / "checkout"
            report_link_checkout.mkdir(parents=True)
            (report_link_root / "agent-output").mkdir()
            report_link_process = report_link_root / "control" / "diagnostic-agent"
            report_link_process.mkdir(parents=True)
            report_target = report_link_root / "control-report"
            report_target.write_text("control\n")
            report_link = report_link_process / "final-message.json"
            report_link.symlink_to(report_target)

            reviewer_output = root / "reviewer-output"
            reviewer_workspace = reviewer_output / "workspace"
            reviewer_workspace.mkdir(parents=True)
            resume = list(
                codex_arguments("prompt", checkout, schema, final_message, "session-1")
            )
            resume[resume.index("--add-dir") + 1] = str(reviewer_workspace)
            resume[resume.index("--output-last-message") + 1] = str(
                reviewer_output / "review.json"
            )
            path_conflicts = {
                "symlink-parent": (
                    codex_arguments(
                        "prompt",
                        symlink_parent_checkout,
                        schema,
                        symlink_parent_process / "final-message.json",
                        None,
                    ),
                    symlink_parent_checkout,
                ),
                "control-parent": (
                    codex_arguments(
                        "prompt",
                        control_parent_checkout,
                        schema,
                        control_parent_process / "final-message.json",
                        None,
                    ),
                    control_parent_checkout,
                ),
                "report-symlink": (
                    codex_arguments(
                        "prompt",
                        report_link_checkout,
                        schema,
                        report_link,
                        None,
                    ),
                    report_link_checkout,
                ),
                "resume-report-symlink": (
                    codex_arguments(
                        "prompt",
                        report_link_checkout,
                        schema,
                        report_link,
                        "session-1",
                    ),
                    report_link_checkout,
                ),
                "resume-reviewer-hybrid": (tuple(resume), reviewer_workspace),
            }
            for name, (arguments, process_cwd) in path_conflicts.items():
                artifact = root / f"artifact-{name}"
                with self.subTest(name=name):
                    with self.assertRaises(ProcessError):
                        run_process(
                            "/usr/local/bin/codex",
                            arguments,
                            process_cwd,
                            artifact,
                            10,
                        )
                    self.assertFalse(artifact.exists())

    def test_process_rejects_codex_contract_text_without_cli_pairs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            artifact = Path(temporary) / "run"
            with self.assertRaisesRegex(
                ProcessError,
                "Codex argv does not match a supported contract",
            ):
                run_process(
                    "/usr/local/bin/codex",
                    (
                        "exec",
                        "--sandbox",
                        "workspace-write",
                        'approval_policy="never" '
                        "sandbox_workspace_write.network_access=false",
                    ),
                    Path(temporary),
                    artifact,
                    10,
                )
            self.assertFalse(artifact.exists())

    def test_process_rejects_unbounded_startup_timeout_before_launch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for value in (float("nan"), float("inf"), float("-inf"), 0.0, -1.0):
                artifact = root / repr(value)
                with (
                    self.subTest(value=value),
                    self.assertRaisesRegex(
                        ProcessError, "startup timeout must be positive and finite"
                    ),
                ):
                    run_process(
                        "/usr/local/bin/fbpython",
                        ("-c", "raise SystemExit(0)"),
                        root,
                        artifact,
                        1,
                        readiness_path=root / "ready",
                        startup_timeout_seconds=value,
                    )
                self.assertFalse(artifact.exists())

    def test_hermit_rejects_unbounded_outer_timeout_inputs(self) -> None:
        argv = tuple(_argv(1))
        guest = Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"])
        controller = Path("/tmp/tfa-phase2/controller")
        for value in (float("nan"), float("inf"), float("-inf"), 0.0, -1.0):
            with (
                self.subTest(value=value),
                self.assertRaisesRegex(
                    HermitError, "outer timeout must be positive and finite"
                ),
            ):
                hermit_attempt_argv(argv, guest, controller, value)

    def test_unbounded_verify_space_refuses_before_payload_launch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            attempt = root / "attempt"
            attempt.mkdir()
            refused = hermit_module._safehermit_log_root(attempt) / (
                f"brs-{hermit_module._verify_allocation_tag(attempt)}-123-456"
            )
            create = {
                "argv": ["bounded-run-space", "create"],
                "returncode": 10,
                "stderr": "UNBOUNDED\n",
                "stdout": f"{refused}\n",
            }
            cleanup = {
                "argv": ["bounded-run-space", "destroy"],
                "returncode": 0,
                "stderr": "",
                "stdout": "",
            }
            with (
                patch(
                    "testinfra.tfa.hermit._bounded_space_result",
                    side_effect=(create, cleanup),
                ),
                patch("testinfra.tfa.hermit.run_process") as process,
                self.assertRaisesRegex(
                    HermitError, "bounded verification space was not applied"
                ),
            ):
                hermit_module.run_hermit_attempt(
                    hermit_witness_argv(tuple(_argv(1)), attempt),
                    Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                    attempt,
                    180,
                )
            process.assert_not_called()
            refusal = json.loads((attempt / "verify-output-refusal.json").read_text())
            self.assertEqual(refusal["create"]["returncode"], 10)
            self.assertEqual(refusal["cleanup"]["returncode"], 0)
            self.assertFalse(refused.exists())

    def test_verify_space_override_accepts_only_private_test_fixture(self) -> None:
        Path("/tmp/tfa-phase2").mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="tfa-production-btrfs-unit-", dir="/tmp/tfa-phase2"
        ) as temporary:
            fixture = Path(temporary)
            mount = fixture / "mount"
            mount.mkdir()
            root = mount / "safehermit"
            root.mkdir(mode=0o700)
            attempt = fixture / "attempt"
            attempt.mkdir()
            with patch.dict(
                os.environ,
                {hermit_module._VERIFY_SPACE_ROOT_ENV: str(root)},
                clear=False,
            ):
                resolved = hermit_module._safehermit_log_root(attempt)
                self.assertEqual(resolved.parent, root)
                replacement = fixture / "replacement"
                replacement.mkdir(mode=0o700)
                root.rmdir()
                root.symlink_to(replacement, target_is_directory=True)
                with self.assertRaisesRegex(HermitError, "not a test fixture"):
                    hermit_module._safehermit_log_root(attempt)

    def test_verify_allocation_failure_boundaries_cleanup_and_preserve_error(
        self,
    ) -> None:
        boundaries = (
            "record",
            "launch",
            "report",
            "copy",
            "destroy",
            "final_evidence",
            "final_transaction",
        )

        class BoundaryError(RuntimeError):
            pass

        for boundary in boundaries:
            with self.subTest(boundary=boundary), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                attempt = root / "attempt"
                attempt.mkdir()
                space = _mock_verify_space(attempt)
                space.verify_dir.mkdir(parents=True)
                expected = BoundaryError(boundary)
                destroy_calls = 0

                def destroy(
                    _space: hermit_module._VerifySpace,
                    _bounded_run_space: Path,
                    _attempt_root: Path,
                    _boundary: str = boundary,
                    _expected: BoundaryError = expected,
                ) -> dict[str, object]:
                    nonlocal destroy_calls
                    destroy_calls += 1
                    if _boundary == "destroy" and destroy_calls == 1:
                        raise _expected
                    _space.verify_dir.rmdir()
                    _space.path.rmdir()
                    return {"returncode": 0}

                original_transaction = hermit_module._write_verify_transaction
                original_metadata = hermit_module.write_metadata

                def transaction(
                    transaction_root: Path,
                    transaction_space: hermit_module._VerifySpace,
                    phase: str,
                    _boundary: str = boundary,
                    _expected: BoundaryError = expected,
                    _original: Callable[..., None] = original_transaction,
                    **details: object,
                ) -> None:
                    if _boundary == "record" and phase == "prepared":
                        raise _expected
                    if _boundary == "final_transaction" and phase == "finalized":
                        raise _expected
                    _original(
                        transaction_root,
                        transaction_space,
                        phase,
                        **details,
                    )

                def metadata(
                    path: Path,
                    value: dict[str, object],
                    _boundary: str = boundary,
                    _expected: BoundaryError = expected,
                    _original: Callable[..., None] = original_metadata,
                ) -> None:
                    if (
                        _boundary == "final_evidence"
                        and path.name == "verify-output.json"
                    ):
                        raise _expected
                    _original(path, value)

                inventory = {
                    "entries": [],
                    "entry_count": 0,
                    "max_bytes": hermit_module._VERIFY_OUTPUT_MAX_BYTES,
                    "max_entries": hermit_module._VERIFY_OUTPUT_MAX_ENTRIES,
                    "root": {"kind": "directory"},
                    "total_bytes": 0,
                }
                result = ProcessResult((), 0, 0, False, root / "out", root / "err")
                with ExitStack() as stack:
                    stack.enter_context(
                        patch(
                            "testinfra.tfa.hermit._prepare_verify_space",
                            return_value=space,
                        )
                    )
                    stack.enter_context(
                        patch(
                            "testinfra.tfa.hermit._destroy_verify_space",
                            side_effect=destroy,
                        )
                    )
                    stack.enter_context(
                        patch(
                            "testinfra.tfa.hermit._write_verify_transaction",
                            side_effect=transaction,
                        )
                    )
                    stack.enter_context(
                        patch(
                            "testinfra.tfa.hermit.write_metadata",
                            side_effect=metadata,
                        )
                    )
                    stack.enter_context(
                        patch(
                            "testinfra.tfa.hermit.run_process",
                            side_effect=expected if boundary == "launch" else None,
                            return_value=result,
                        )
                    )
                    stack.enter_context(
                        patch(
                            "testinfra.tfa.hermit._bounded_space_report",
                            side_effect=expected if boundary == "report" else None,
                            return_value={"returncode": 0},
                        )
                    )
                    stack.enter_context(
                        patch(
                            "testinfra.tfa.hermit._verify_output_inventory",
                            side_effect=expected if boundary == "copy" else None,
                            return_value=inventory,
                        )
                    )
                    with self.assertRaises(BoundaryError) as caught:
                        hermit_module.run_hermit_attempt(
                            hermit_witness_argv(tuple(_argv(1)), attempt),
                            Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                            attempt,
                            180,
                        )
                self.assertIs(caught.exception, expected)
                self.assertFalse(space.path.exists())
                transaction_value = json.loads(
                    (attempt / "verify-output-transaction.json").read_text()
                )
                self.assertEqual(transaction_value["phase"], "recovered")
                self.assertEqual(
                    destroy_calls,
                    2 if boundary == "destroy" else 1,
                )

    def test_verify_allocation_create_inspection_failure_cleans_exact_path(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            attempt = root / "attempt"
            attempt.mkdir()
            bounded_root = hermit_module._safehermit_log_root(attempt)
            path = bounded_root / (
                f"brs-{hermit_module._verify_allocation_tag(attempt)}-123-456"
            )
            receipt: dict[str, object] = {
                "argv": ["bounded-run-space", "create"],
                "returncode": 0,
                "stderr": "",
                "stdout": f"{path}\n",
            }
            space = replace(
                _mock_verify_space(attempt),
                path=path,
                verify_dir=path / "verify",
            )
            expected = RuntimeError("create-inspection")

            def create(
                _executable: Path, arguments: list[str], _cwd: Path
            ) -> dict[str, object]:
                self.assertEqual(arguments[0], "create")
                path.mkdir()
                return receipt

            def destroy(
                _space: hermit_module._VerifySpace,
                _bounded_run_space: Path,
                _attempt_root: Path,
            ) -> dict[str, object]:
                path.rmdir()
                return {"returncode": 0}

            with (
                patch(
                    "testinfra.tfa.hermit._bounded_space_result",
                    side_effect=create,
                ),
                patch(
                    "testinfra.tfa.hermit._inspect_verify_space",
                    side_effect=(expected, space),
                ),
                patch(
                    "testinfra.tfa.hermit._destroy_verify_space",
                    side_effect=destroy,
                ) as cleanup,
                patch("testinfra.tfa.hermit.run_process") as process,
                self.assertRaises(RuntimeError) as caught,
            ):
                hermit_module.run_hermit_attempt(
                    hermit_witness_argv(tuple(_argv(1)), attempt),
                    Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                    attempt,
                    180,
                )
            self.assertIs(caught.exception, expected)
            cleanup.assert_called_once()
            process.assert_not_called()
            self.assertFalse(path.exists())

    def test_verify_allocation_lock_wait_is_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            lock = Path(temporary) / "allocation.lock"
            ready, child_ready = os.pipe()
            release, child_release = os.pipe()
            holder = os.fork()
            if holder == 0:
                os.close(ready)
                os.close(child_release)
                descriptor = os.open(lock, os.O_RDWR | os.O_CREAT, 0o600)
                fcntl.lockf(descriptor, fcntl.LOCK_EX)
                os.write(child_ready, b"ready")
                os.read(release, 1)
                os.close(descriptor)
                os._exit(0)
            os.close(child_ready)
            os.close(release)
            try:
                self.assertEqual(os.read(ready, 5), b"ready")
                with (
                    patch(
                        "testinfra.tfa.hermit._verify_allocation_lock",
                        return_value=lock,
                    ),
                    patch(
                        "testinfra.tfa.hermit._VERIFY_ALLOCATION_LOCK_TIMEOUT_SECONDS",
                        0,
                    ),
                    self.assertRaisesRegex(
                        HermitError, "verification allocation lock timed out"
                    ),
                ):
                    with hermit_module._verify_allocation_guard(Path(temporary)):
                        self.fail("contended allocation lock was acquired")
            finally:
                os.close(ready)
                os.write(child_release, b"x")
                os.close(child_release)
                self.assertEqual(os.waitpid(holder, 0)[1], 0)

    def test_verify_output_inventory_enforces_count_byte_and_type_caps(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            byte_source = root / "byte-source"
            byte_source.mkdir()
            (byte_source / "oversize").write_bytes(b"four")
            with (
                patch("testinfra.tfa.hermit._VERIFY_OUTPUT_MAX_BYTES", 3),
                self.assertRaisesRegex(HermitError, "exceeds its cap"),
            ):
                hermit_module._verify_output_inventory(byte_source)

            count_source = root / "count-source"
            count_source.mkdir()
            (count_source / "one").write_text("1")
            (count_source / "two").write_text("2")
            with (
                patch("testinfra.tfa.hermit._VERIFY_OUTPUT_MAX_ENTRIES", 1),
                self.assertRaisesRegex(HermitError, "entry cap exceeded"),
            ):
                hermit_module._verify_output_inventory(count_source)

            type_source = root / "type-source"
            type_source.mkdir()
            (type_source / "link").symlink_to(byte_source / "oversize")
            with self.assertRaisesRegex(HermitError, "unsupported type"):
                hermit_module._verify_output_inventory(type_source)

            mount_source = root / "mount-source"
            (mount_source / "nested").mkdir(parents=True)
            with (
                patch(
                    "testinfra.tfa.hermit._fd_mount_id",
                    side_effect=(1, 2),
                ),
                self.assertRaisesRegex(HermitError, "crossed a mount boundary"),
            ):
                hermit_module._verify_output_inventory(mount_source)

    def test_safehermit_rejects_not_applied_disk_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            controller = root / "controller-cwd"
            controller.mkdir()
            argv = hermit_attempt_argv(
                hermit_witness_argv(tuple(_argv(1)), root),
                Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                controller,
                180,
                safehermit_wrapper_path(root),
            )
            metadata, argv = _write_safehermit_fixture(
                root, argv, 0, _loaded_case().hermit_sha256
            )
            report = root / "safehermit.json"
            report.write_text(
                report.read_text().replace(
                    "safehermit: bound.disk=APPLIED:8G for log_dir only; "
                    "guest paths and explicit log files elsewhere are outside "
                    "this bound",
                    "safehermit: bound.disk=NOT_APPLIED:bounded-run-space reported "
                    "UNBOUNDED (rc=10); space exists but is not capped",
                )
            )
            metadata["finished_wall_time_ns"] = time.time_ns()
            with self.assertRaisesRegex(
                HermitError, "safety or completion evidence differs"
            ):
                safehermit_evidence(
                    argv, metadata, report, _loaded_case().hermit_sha256
                )

    def test_safehermit_rejects_verify_output_tamper_and_quota_mismatch(self) -> None:
        for mutation in ("output", "quota", "command"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                controller = root / "controller-cwd"
                controller.mkdir()
                argv = hermit_attempt_argv(
                    hermit_witness_argv(tuple(_argv(1)), root),
                    Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                    controller,
                    180,
                    safehermit_wrapper_path(root),
                )
                case = _loaded_case()
                metadata, argv = _write_safehermit_fixture(
                    root, argv, 0, case.hermit_sha256
                )
                if mutation == "output":
                    (root / "verify" / "summary.txt").write_text("tampered\n")
                    message = "differs from its inventory"
                else:
                    evidence_path = root / "verify-output.json"
                    evidence = json.loads(evidence_path.read_text())
                    if mutation == "quota":
                        evidence["report"]["limit_bytes"] = 7
                        write_metadata(evidence_path, evidence)
                        message = "evidence is incomplete"
                    else:
                        metadata_path = Path(evidence["create"]["process"]["path"])
                        metadata = json.loads(metadata_path.read_text())
                        metadata["containment_empty"] = False
                        write_metadata(metadata_path, metadata)
                        message = "command evidence differs"
                with self.assertRaisesRegex(HermitError, message):
                    safehermit_evidence(
                        argv,
                        metadata,
                        root / "safehermit.json",
                        case.hermit_sha256,
                    )

    def test_preserves_argv_tokens(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            result = run_process(
                "/usr/local/bin/fbpython",
                (
                    "-c",
                    "import json,sys; print(json.dumps(sys.argv[1:]))",
                    "a b",
                    "$HOME",
                    "x;y",
                ),
                Path(temporary),
                Path(temporary) / "run",
                10,
            )
            self.assertEqual(result.returncode, 0, result.stderr_path.read_text())
            output = json.loads(result.stdout_path.read_text())
            self.assertEqual(output, ["a b", "$HOME", "x;y"])

    def test_hard_timeout_writes_resumable_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            artifact_dir = Path(temporary) / "run"
            result = run_process(
                "/usr/local/bin/fbpython",
                ("-c", "import time; time.sleep(5)"),
                Path(temporary),
                artifact_dir,
                0.1,
            )
            metadata = json.loads((artifact_dir / "metadata.json").read_text())
            self.assertTrue(result.timed_out)
            self.assertEqual(metadata["state"], "timeout")
            self.assertEqual(metadata["process_group"], result.process_group)

    def test_timeout_kills_nested_cgroup_descendant(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lifetime = root / "grandchild-lifetime"
            ready = root / "grandchild-started"
            grandchild = (
                "import fcntl,pathlib,time; "
                "deadline=time.monotonic()+1; "
                "cgroup=pathlib.Path('/proc/self/cgroup'); "
                "\nwhile '/migrated' not in cgroup.read_text() "
                "and time.monotonic()<deadline: time.sleep(0.01)\n"
                f"lifetime=open({str(lifetime)!r},'w'); "
                "fcntl.flock(lifetime,fcntl.LOCK_EX); "
                f"pathlib.Path({str(ready)!r}).write_text(cgroup.read_text()); "
                "time.sleep(30)"
            )
            script = (
                "import pathlib,subprocess,sys,time; "
                "relative=pathlib.Path('/proc/self/cgroup').read_text()"
                ".strip().removeprefix('0::/'); "
                "migrated=pathlib.Path('/sys/fs/cgroup')/relative/'migrated'; "
                "migrated.mkdir(); "
                f"child=subprocess.Popen(['/usr/bin/python3','-c',{grandchild!r}],"
                "start_new_session=True); "
                "(migrated/'cgroup.procs').write_text(str(child.pid)); "
                f"ready=pathlib.Path({str(ready)!r}); "
                "deadline=time.monotonic()+1; "
                "\nwhile not ready.exists() and time.monotonic()<deadline: "
                "time.sleep(0.01)\n"
                "time.sleep(30)"
            )
            result = run_process(
                "/usr/local/bin/fbpython",
                ("-c", script),
                root,
                root / "run",
                # One deadline origin covers readiness and execution, so the
                # grandchild must become ready well inside the execution bound.
                5,
                readiness_path=ready,
                startup_timeout_seconds=5,
            )
            self.assertTrue(result.timed_out)
            self.assertTrue(ready.exists())
            self.assertIn("/payload/migrated", ready.read_text())
            with lifetime.open("r+") as lifetime_stream:
                fcntl.flock(lifetime_stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertIsNotNone(result.process_group)
            self.assertEqual(result.containment, CONTAINMENT)

    def test_success_kills_nested_cgroup_descendant(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lifetime = root / "descendant-lifetime"
            ready = root / "descendant-started"
            descendant = (
                "import fcntl,pathlib,time; "
                "deadline=time.monotonic()+1; "
                "cgroup=pathlib.Path('/proc/self/cgroup'); "
                "\nwhile '/migrated' not in cgroup.read_text() "
                "and time.monotonic()<deadline: time.sleep(0.01)\n"
                f"lifetime=open({str(lifetime)!r},'w'); "
                "fcntl.flock(lifetime,fcntl.LOCK_EX); "
                f"pathlib.Path({str(ready)!r}).write_text(cgroup.read_text()); "
                "time.sleep(30)"
            )
            script = (
                "import pathlib,subprocess,sys,time; "
                "relative=pathlib.Path('/proc/self/cgroup').read_text()"
                ".strip().removeprefix('0::/'); "
                "migrated=pathlib.Path('/sys/fs/cgroup')/relative/'migrated'; "
                "migrated.mkdir(); "
                f"child=subprocess.Popen(['/usr/bin/python3','-c',{descendant!r}],"
                "start_new_session=True); "
                "(migrated/'cgroup.procs').write_text(str(child.pid)); "
                f"ready=pathlib.Path({str(ready)!r}); "
                "deadline=time.monotonic()+1; "
                "\nwhile not ready.exists() and time.monotonic()<deadline: "
                "time.sleep(0.01)\n"
            )
            result = run_process(
                "/usr/local/bin/fbpython",
                ("-c", script),
                root,
                root / "run",
                10,
            )
            self.assertEqual(result.returncode, 0)
            self.assertTrue(ready.exists())
            self.assertIn("/payload/migrated", ready.read_text())
            self.assertGreaterEqual(result.descendants_terminated, 1)
            with lifetime.open("r+") as lifetime_stream:
                fcntl.flock(lifetime_stream, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_failure_kills_nested_cgroup_descendant(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lifetime = root / "descendant-lifetime"
            ready = root / "descendant-started"
            descendant = (
                "import fcntl,pathlib,time; "
                "deadline=time.monotonic()+1; "
                "cgroup=pathlib.Path('/proc/self/cgroup'); "
                "\nwhile '/migrated' not in cgroup.read_text() "
                "and time.monotonic()<deadline: time.sleep(0.01)\n"
                f"lifetime=open({str(lifetime)!r},'w'); "
                "fcntl.flock(lifetime,fcntl.LOCK_EX); "
                f"pathlib.Path({str(ready)!r}).write_text(cgroup.read_text()); "
                "time.sleep(30)"
            )
            script = (
                "import os,pathlib,signal,subprocess,sys,time; "
                "relative=pathlib.Path('/proc/self/cgroup').read_text()"
                ".strip().removeprefix('0::/'); "
                "migrated=pathlib.Path('/sys/fs/cgroup')/relative/'migrated'; "
                "migrated.mkdir(); "
                f"child=subprocess.Popen(['/usr/bin/python3','-c',{descendant!r}],"
                "start_new_session=True); "
                "(migrated/'cgroup.procs').write_text(str(child.pid)); "
                f"ready=pathlib.Path({str(ready)!r}); "
                "deadline=time.monotonic()+1; "
                "\nwhile not ready.exists() and time.monotonic()<deadline: "
                "time.sleep(0.01)\n"
                "os.kill(os.getpid(),signal.SIGKILL)"
            )
            result = run_process(
                "/usr/local/bin/fbpython",
                ("-c", script),
                root,
                root / "run",
                10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(ready.exists())
            self.assertIn("/payload/migrated", ready.read_text())
            self.assertGreaterEqual(result.descendants_terminated, 1)
            with lifetime.open("r+") as lifetime_stream:
                fcntl.flock(lifetime_stream, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_resume_arguments_and_session_readback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            stdout = Path(temporary) / "stdout.txt"
            stdout.write_text('{"thread_id":"session-1"}\n')
            arguments = codex_arguments(
                "continue",
                Path(temporary) / "checkout",
                Path(temporary) / "schema.json",
                Path(temporary)
                / "case"
                / "control"
                / "diagnostic-agent"
                / "final-message.json",
                session_id_from_jsonl(stdout),
            )
            self.assertEqual(arguments[0], "exec")
            self.assertIn("resume", arguments)
            self.assertLess(arguments.index("--sandbox"), arguments.index("resume"))
            self.assertIn("session-1", arguments)
            self.assertIn("--skip-git-repo-check", arguments)

    def test_fresh_agent_uses_compatible_approval_sandbox_contract(self) -> None:
        report = Path("/artifacts/case/control/diagnostic-agent/final-message.json")
        arguments = codex_arguments(
            "start",
            Path("/checkout"),
            Path("/schema.json"),
            report,
            None,
        )
        self.assertNotIn("--approve-for-me", arguments)
        self.assertIn("workspace-write", arguments)
        self.assertIn('approval_policy="never"', arguments)
        self.assertIn('shell_environment_policy.inherit="none"', arguments)
        self.assertIn("sandbox_workspace_write.network_access=false", arguments)
        self.assertIn("sandbox_workspace_write.exclude_tmpdir_env_var=true", arguments)
        self.assertIn("sandbox_workspace_write.exclude_slash_tmp=true", arguments)
        self.assertIn("--skip-git-repo-check", arguments)
        self.assertIn("--ignore-user-config", arguments)
        self.assertIn("--ignore-rules", arguments)
        self.assertEqual(
            arguments[arguments.index("--add-dir") + 1],
            "/artifacts/case/agent-output",
        )
        self.assertEqual(
            arguments[arguments.index("--output-last-message") + 1], str(report)
        )

    def test_diagnostic_environment_excludes_identity_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log_root = Path(temporary) / "safehermit-logs"
            environment = diagnostic_environment(log_root)
            self.assertNotIn("SSH_AUTH_SOCK", environment)
            self.assertNotIn("THRIFT_TLS_CL_CERT_PATH", environment)
            self.assertNotIn("META_ACCESS_TOKEN", environment)
            self.assertEqual(environment["PATH"], "/usr/local/bin:/usr/bin:/bin")
            self.assertEqual(environment["SAFEHERMIT_LOG_ROOT"], str(log_root))
            self.assertTrue(log_root.is_dir())

    @_serialized_unit_working_directory
    def test_witness_adds_controller_cwd_without_changing_guest_cwd(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            attempt_root = root / "asyncssl" / "pass"
            space = _mock_verify_space(attempt_root)
            self.assertIsInstance(space, hermit_module._VerifySpace)
            expected_argv = hermit_module._current_verify_argv(
                hermit_attempt_argv(
                    hermit_module._replace_verify_log_dir(
                        hermit_witness_argv(tuple(_argv(1)), attempt_root),
                        attempt_root / "verify",
                        space.verify_dir,
                    ),
                    Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                    attempt_root / "controller-cwd",
                    180,
                    safehermit_wrapper_path(attempt_root),
                ),
            )
            expected = ProcessResult(
                expected_argv, 0, 0.1, False, root / "stdout", root / "stderr"
            )
            with (
                patch(
                    "testinfra.tfa.hermit._prepare_verify_space",
                    return_value=space,
                ),
                patch(
                    "testinfra.tfa.hermit._finalize_verify_space",
                    return_value={},
                ),
                patch(
                    "testinfra.tfa.hermit.run_process", return_value=expected
                ) as process,
            ):
                result = run_witness(
                    load_manifest(manifest_path),
                    "asyncssl",
                    "pass",
                    root,
                    180,
                )
            self.assertEqual(result.argv, expected_argv)
            self.assertTrue(Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]).is_dir())
            cwd_record = json.loads(
                (root / "asyncssl" / "pass" / "working-directory.json").read_text()
            )
            self.assertEqual(
                cwd_record["working_directory"],
                EXPECTED_WORKING_DIRECTORIES["asyncssl"],
            )
            process.assert_called_once()
            process_arguments = process.call_args.args
            self.assertEqual(
                process_arguments[:5],
                (
                    expected_argv[0],
                    expected_argv[1:],
                    attempt_root / "controller-cwd",
                    attempt_root / "wrapper",
                    180,
                ),
            )
            environment = process_arguments[5]
            self.assertIsInstance(environment, dict)
            self.assertEqual(
                environment["SAFEHERMIT_LOG_ROOT"],
                str(hermit_module._safehermit_log_root(attempt_root)),
            )
            self.assertEqual(environment["TMPDIR"], str(space.verify_dir))
            self.assertIn(
                f"--workdir={EXPECTED_WORKING_DIRECTORIES['asyncssl']}",
                expected_argv,
            )
            self.assertNotIn("--timeout=110", expected_argv)
            self.assertEqual(
                expected_argv[expected_argv.index("--sh-deadline") + 1], "110"
            )

    def test_safehermit_wrapper_pin_rejects_hash_and_replacement(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "safehermit"
            destination = root / "pinned"
            source.write_bytes(b"#!/bin/sh\nexit 0\n")
            source.chmod(0o755)
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            with self.assertRaisesRegex(HermitError, "wrapper hash differs"):
                _pin_safehermit(source, destination, "0" * 64)
            self.assertFalse(destination.exists())
            real_read = os.read
            replaced = False

            def replace_after_read(descriptor: int, size: int) -> bytes:
                nonlocal replaced
                data = real_read(descriptor, size)
                if data and not replaced:
                    replaced = True
                    source.unlink()
                    source.write_bytes(b"#!/bin/sh\nexit 1\n")
                    source.chmod(0o755)
                return data

            with (
                patch(
                    "testinfra.tfa.hermit.os.read",
                    side_effect=replace_after_read,
                ),
                self.assertRaisesRegex(HermitError, "changed while it was pinned"),
            ):
                _pin_safehermit(source, destination, digest)
            self.assertFalse(destination.exists())

    @_serialized_unit_working_directory
    def test_witness_rejects_preexisting_safehermit_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            attempt_root = root / "asyncssl" / "pass"
            attempt_root.mkdir(parents=True)
            (attempt_root / "safehermit.json").write_text("stale\n")
            with (
                patch("testinfra.tfa.hermit.run_process") as process,
                self.assertRaisesRegex(HermitError, "report is not fresh"),
            ):
                run_witness(
                    load_manifest(manifest_path),
                    "asyncssl",
                    "pass",
                    root,
                    180,
                )
            process.assert_not_called()

    @_serialized_unit_working_directory
    def test_seed_characterization_rewrites_only_seed_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            expected = ProcessResult(
                hermit_attempt_argv(
                    tuple(_argv(9)),
                    Path(EXPECTED_WORKING_DIRECTORIES["asyncssl"]),
                    root / "asyncssl" / "seed-9" / "controller-cwd",
                    180,
                    safehermit_wrapper_path(root / "asyncssl" / "seed-9"),
                ),
                0,
                0.1,
                False,
                root / "stdout",
                root / "stderr",
            )
            attempt_root = root / "asyncssl" / "seed-9"
            space = _mock_verify_space(attempt_root)
            with (
                patch(
                    "testinfra.tfa.hermit._prepare_verify_space",
                    return_value=space,
                ),
                patch(
                    "testinfra.tfa.hermit._finalize_verify_space",
                    return_value={},
                ),
                patch(
                    "testinfra.tfa.hermit.run_process", return_value=expected
                ) as process,
            ):
                run_seed(load_manifest(manifest_path), "asyncssl", 9, root, 180)
            call = process.call_args.args
            argv = (call[0], *call[1])
            self.assertEqual(argv[argv.index("--sched-seed") + 1], "9")
            self.assertEqual(call[2], root / "asyncssl" / "seed-9" / "controller-cwd")
            self.assertTrue(
                any(token.endswith("/asyncssl/seed-9/safehermit.txt") for token in argv)
            )


def _load_payload_wrapper() -> types.ModuleType:
    path = Path(process_module.__file__).with_name("payload_wrapper.py")
    spec = importlib.util.spec_from_file_location("tfa_payload_wrapper_test", path)
    if spec is None or spec.loader is None:
        raise AssertionError("payload wrapper source cannot be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _open_descriptor_count() -> int:
    return len(os.listdir("/proc/self/fd"))


class _FakeClock:
    def __init__(self, now: float) -> None:
        self.now = now

    def monotonic(self) -> float:
        return self.now

    def time_ns(self) -> int:
        return time.time_ns()

    def sleep(self, seconds: float) -> None:
        self.now += seconds


class CodexIdentityHardeningTest(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        os.chmod(self.root, 0o755)
        for name, value in (
            ("_TRUSTED_OWNER_UID", os.getuid()),
            ("_TRUSTED_ROOT", self.root),
        ):
            patcher = patch.object(process_module, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def _trusted_file(
        self, relative: str, content: bytes = b"codex\n", mode: int = 0o755
    ) -> Path:
        path = self.root / relative
        path.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
        path.write_bytes(content)
        os.chmod(path, mode)
        return path

    def test_trusted_record_accepts_owner_only_file_as_control(self) -> None:
        path = self._trusted_file("bin/codex")
        record = process_module._root_owned_regular_record(path, "executable")
        self.assertEqual(record["path"], str(path))
        self.assertEqual(record["sha256"], hashlib.sha256(b"codex\n").hexdigest())
        self.assertEqual(record["owner_uid"], os.getuid())
        self.assertEqual(record["link_count"], 1)

    def test_trusted_record_rejects_foreign_owner(self) -> None:
        path = self._trusted_file("bin/codex")
        with (
            patch.object(process_module, "_TRUSTED_OWNER_UID", os.getuid() + 1),
            self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"),
        ):
            process_module._root_owned_regular_record(path, "executable")

    def test_trusted_record_rejects_additional_hard_link(self) -> None:
        path = self._trusted_file("bin/codex")
        alias = path.with_name("alias")
        os.link(path, alias)
        for candidate in (path, alias):
            with self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"):
                process_module._root_owned_regular_record(candidate, "executable")

    def test_trusted_record_rejects_group_or_world_writable_mode(self) -> None:
        for mode in (0o775, 0o757, 0o664, 0o646):
            with self.subTest(mode=oct(mode)):
                path = self._trusted_file(f"bin/codex-{mode:o}", mode=mode)
                with self.assertRaisesRegex(
                    ProcessError, "ownership or mode is unsafe"
                ):
                    process_module._root_owned_regular_record(path, "executable")

    def test_trusted_record_rejects_symlink(self) -> None:
        target = self._trusted_file("bin/codex")
        link = target.with_name("link")
        link.symlink_to(target)
        with self.assertRaisesRegex(
            ProcessError, "Codex executable (is unavailable|ownership or mode)"
        ):
            process_module._root_owned_regular_record(link, "executable")

    def test_trusted_record_rejects_writable_parent_directory(self) -> None:
        path = self._trusted_file("shared/bin/codex")
        process_module._root_owned_regular_record(path, "executable")
        for mode in (0o777, 0o775, 0o757):
            with self.subTest(mode=oct(mode)):
                os.chmod(self.root / "shared", mode)
                with self.assertRaisesRegex(
                    ProcessError, "executable directory chain is unsafe"
                ):
                    process_module._root_owned_regular_record(path, "executable")
        os.chmod(self.root / "shared", 0o755)

    def test_trusted_record_rejects_symlinked_parent_directory(self) -> None:
        self._trusted_file("real/codex")
        (self.root / "alias").symlink_to(self.root / "real")
        with self.assertRaisesRegex(
            ProcessError, "executable directory chain is unsafe"
        ):
            process_module._root_owned_regular_record(
                self.root / "alias" / "codex", "executable"
            )

    def test_directory_chain_rejects_foreign_owner_and_escapes(self) -> None:
        directory = self.root / "bin"
        directory.mkdir(mode=0o755)
        process_module._require_trusted_directory_chain(directory, "executable")
        with (
            patch.object(process_module, "_TRUSTED_OWNER_UID", os.getuid() + 1),
            self.assertRaisesRegex(ProcessError, "directory chain is unsafe"),
        ):
            process_module._require_trusted_directory_chain(directory, "executable")
        for candidate in (
            Path("bin"),
            self.root / "bin" / ".." / "bin",
            self.root.parent,
        ):
            with (
                self.subTest(candidate=str(candidate)),
                self.assertRaisesRegex(ProcessError, "directory chain is unsafe"),
            ):
                process_module._require_trusted_directory_chain(candidate, "executable")
        with self.assertRaisesRegex(ProcessError, "directory chain is unavailable"):
            process_module._require_trusted_directory_chain(
                self.root / "missing", "executable"
            )

    def test_release_manifest_requires_exact_channel_and_identity(self) -> None:
        stable = self._trusted_file("cli/codex.real", b"stable\n")
        latest = self._trusted_file("cli/codex_latest.real", b"latest\n")
        manifests = {
            "stable": (stable, hashlib.sha256(b"stable\n").hexdigest()),
            "latest": (latest, hashlib.sha256(b"latest\n").hexdigest()),
        }
        with patch.object(process_module, "_CODEX_RELEASE_MANIFESTS", manifests):
            for environment, channel, path in (
                ({}, "stable", stable),
                ({"META_CODEX_RELEASE": ""}, "stable", stable),
                ({"META_CODEX_RELEASE": "latest"}, "latest", latest),
            ):
                with self.subTest(environment=environment):
                    selected, record = process_module._codex_release_identity(
                        environment
                    )
                    self.assertEqual(selected, channel)
                    self.assertEqual(record["path"], str(path))
            for requested in (
                "stable",
                "LATEST",
                " latest",
                "latest\n",
                "0.157.0",
            ):
                with (
                    self.subTest(requested=requested),
                    self.assertRaisesRegex(
                        ProcessError, "release channel is not reviewed"
                    ),
                ):
                    process_module._codex_release_identity(
                        {"META_CODEX_RELEASE": requested}
                    )

    def test_release_manifest_rejects_digest_path_mode_and_link_drift(self) -> None:
        stable = self._trusted_file("cli/codex.real", b"stable\n")
        digest = hashlib.sha256(b"stable\n").hexdigest()
        with patch.object(
            process_module, "_CODEX_RELEASE_MANIFESTS", {"stable": (stable, "0" * 64)}
        ):
            with self.assertRaisesRegex(ProcessError, "manifest identity differs"):
                process_module._codex_release_identity({})
        with patch.object(
            process_module, "_CODEX_RELEASE_MANIFESTS", {"stable": (stable, digest)}
        ):
            process_module._codex_release_identity({})
            with patch.object(
                process_module,
                "regular_file_record",
                return_value={
                    "kind": "file",
                    "mode": 0o755,
                    "path": str(stable.with_name("other")),
                    "sha256": digest,
                    "size": 7,
                },
            ):
                with self.assertRaisesRegex(ProcessError, "manifest identity differs"):
                    process_module._codex_release_identity({})
            os.chmod(stable, 0o664)
            with self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"):
                process_module._codex_release_identity({})
            os.chmod(stable, 0o755)
            os.link(stable, stable.with_name("codex.real.alias"))
            with self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"):
                process_module._codex_release_identity({})

    def test_entrypoint_requires_trusted_symlink(self) -> None:
        target = self._trusted_file("cli/codex")
        entrypoint = self.root / "bin" / "codex"
        entrypoint.parent.mkdir(mode=0o755)
        entrypoint.symlink_to(target)
        with patch.object(process_module, "_CODEX_EXECUTABLE", str(entrypoint)):
            identity = process_module._codex_entrypoint_identity()
            self.assertEqual(identity["resolved_path"], str(target))
            with (
                patch.object(process_module, "_TRUSTED_OWNER_UID", os.getuid() + 1),
                self.assertRaisesRegex(ProcessError, "ownership or type is unsafe"),
            ):
                process_module._codex_entrypoint_identity()
            os.chmod(entrypoint.parent, 0o777)
            with self.assertRaisesRegex(
                ProcessError, "entrypoint directory chain is unsafe"
            ):
                process_module._codex_entrypoint_identity()
            os.chmod(entrypoint.parent, 0o755)
            entrypoint.unlink()
            entrypoint.write_bytes(b"not a symlink\n")
            with self.assertRaisesRegex(ProcessError, "ownership or type is unsafe"):
                process_module._codex_entrypoint_identity()

    def test_launcher_descriptor_rejects_changed_identity(self) -> None:
        path = self._trusted_file("cli/codex")
        record = process_module._root_owned_regular_record(path, "executable")
        descriptor, launch_path = process_module._open_verified_codex_launcher(record)
        os.close(descriptor)
        self.assertEqual(launch_path, str(path))
        before = _open_descriptor_count()
        os.chmod(path, 0o700)
        with self.assertRaisesRegex(ProcessError, "changed before launch"):
            process_module._open_verified_codex_launcher(record)
        os.chmod(path, 0o755)
        with path.open("ab") as stream:
            stream.write(b"grown\n")
        with self.assertRaisesRegex(ProcessError, "changed before launch"):
            process_module._open_verified_codex_launcher(record)
        replacement = path.with_name("replacement")
        replacement.write_bytes(b"codex\n")
        os.chmod(replacement, 0o755)
        os.replace(replacement, path)
        with self.assertRaisesRegex(ProcessError, "changed before launch"):
            process_module._open_verified_codex_launcher(record)
        link = path.with_name("link")
        link.symlink_to(path)
        with self.assertRaisesRegex(ProcessError, "cannot be pinned"):
            process_module._open_verified_codex_launcher({**record, "path": str(link)})
        self.assertEqual(_open_descriptor_count(), before)

    def _preflight_with_entry(
        self, value: tuple[str, str | None, str | None]
    ) -> tuple[dict[str, object], ProcessError | None]:
        executable = {
            "kind": "file",
            "path": "/usr/local/bin/codex_cli/codex",
            "sha256": "0" * 64,
        }
        manifest = {"path": "/usr/local/bin/codex_cli/codex.real", "sha256": "1" * 64}
        key = ("codex-cli 9.9.9", "0" * 64, "1" * 64, "stable")
        version = MagicMock(returncode=0, stdout="codex-cli 9.9.9\n", stderr="")
        output = self.root / f"preflight-{uuid.uuid4().hex}"
        error: ProcessError | None = None
        with (
            patch.dict(process_module._CODEX_COMPATIBILITY, {key: value}),
            patch.object(TrustedSubprocessWithList, "run", return_value=version),
            patch(
                "testinfra.tfa.process._codex_entrypoint_identity",
                return_value={"resolved_path": executable["path"]},
            ),
            patch(
                "testinfra.tfa.process._root_owned_regular_record",
                return_value=executable,
            ),
            patch(
                "testinfra.tfa.process._codex_release_identity",
                return_value=("stable", manifest),
            ),
        ):
            try:
                process_module._codex_environment_preflight(self.root, output, {})
            except ProcessError as raised:
                error = raised
        self.assertNotIn(key, process_module._CODEX_COMPATIBILITY)
        return json.loads((output / "identity.json").read_text()), error

    def test_eligible_entry_requires_hash_bound_payload(self) -> None:
        for payload in (None, "", "A" * 64, "0" * 63, "g" * 64):
            with self.subTest(payload=payload):
                identity, error = self._preflight_with_entry(
                    ("eligible", None, payload)
                )
                self.assertIsNotNone(error)
                self.assertEqual(identity["state"], "blocked")
                self.assertEqual(
                    identity["blocker"],
                    "eligible Codex compatibility entry lacks a hash-bound payload",
                )
                self.assertIsNone(identity["payload_sha256"])
        identity, error = self._preflight_with_entry(("eligible", None, "a" * 64))
        self.assertIsNone(error)
        self.assertEqual(identity["state"], "eligible")
        self.assertEqual(identity["payload_sha256"], "a" * 64)

    def test_production_compatibility_entries_stay_blocked_and_unbound(self) -> None:
        self.assertEqual(len(process_module._CODEX_COMPATIBILITY), 2)
        for key, value in process_module._CODEX_COMPATIBILITY.items():
            with self.subTest(key=key):
                self.assertEqual(
                    value,
                    ("blocked", process_module._CODEX_SOCKET_ISOLATION_BLOCKER, None),
                )
                _assert_socket_isolation_blocker_text(self, value[1])

    def test_preflight_requires_banner_of_requested_channel(self) -> None:
        executable = {"path": "/usr/local/bin/codex_cli/codex", "sha256": "0" * 64}
        for channel, stderr in (
            ("latest", ""),
            ("latest", process_module._CODEX_VERSION_STDERR["stable"][1]),
            ("stable", process_module._CODEX_VERSION_STDERR["latest"][0]),
        ):
            with self.subTest(channel=channel, stderr=stderr):
                output = self.root / f"preflight-{uuid.uuid4().hex}"
                version = MagicMock(
                    returncode=0, stdout="codex-cli 0.156.1\n", stderr=stderr
                )
                with (
                    patch.object(
                        TrustedSubprocessWithList, "run", return_value=version
                    ),
                    patch(
                        "testinfra.tfa.process._codex_entrypoint_identity",
                        return_value={"resolved_path": executable["path"]},
                    ),
                    patch(
                        "testinfra.tfa.process._root_owned_regular_record",
                        return_value=executable,
                    ),
                    patch(
                        "testinfra.tfa.process._codex_release_identity",
                        return_value=(channel, {"sha256": "1" * 64}),
                    ),
                    self.assertRaisesRegex(ProcessError, "invalid result"),
                ):
                    process_module._codex_environment_preflight(self.root, output, {})
                identity = json.loads((output / "identity.json").read_text())
                self.assertEqual(identity["state"], "version-failed")
                self.assertEqual(identity["release_channel"], channel)

    def test_payload_binding_requires_eligible_hash_bound_identity(self) -> None:
        binding = process_module._codex_payload_binding
        self.assertEqual(binding(None, ("--version",)), "unbound")
        eligible = {"blocker": None, "payload_sha256": "a" * 64, "state": "eligible"}
        self.assertEqual(binding(eligible, ("exec",)), "a" * 64)
        with self.assertRaisesRegex(ProcessError, "lacks a preflight identity"):
            binding(None, ("exec", "prompt"))
        for identity in (
            {**eligible, "state": "blocked"},
            {**eligible, "blocker": "blocked"},
            {**eligible, "payload_sha256": None},
            {**eligible, "payload_sha256": "A" * 64},
            {**eligible, "payload_sha256": "unbound"},
        ):
            with (
                self.subTest(identity=identity),
                self.assertRaisesRegex(ProcessError, "eligible hash-bound payload"),
            ):
                binding(identity, ("exec",))

    def test_spawn_refuses_unbound_codex_identity_before_popen(self) -> None:
        runtime = MagicMock(wrapper=Path("/wrapper"), supervisor=Path("/supervisor"))
        blocked = {
            "blocker": process_module._CODEX_SOCKET_ISOLATION_BLOCKER,
            "payload_sha256": None,
            "state": "blocked",
        }
        for identity, environment, message in (
            (blocked, {}, "eligible hash-bound payload"),
            (None, {}, "lacks a preflight identity"),
            (
                {**blocked, "blocker": None, "state": "eligible"},
                {"TFA_PAYLOAD_SHA256": "a" * 64},
                "already populated",
            ),
        ):
            with (
                self.subTest(message=message),
                patch.object(process_module, "_pinned_runtime", return_value=runtime),
                patch.object(TrustedSubprocessWithList, "Popen") as popen,
                patch.object(process_module, "_open_verified_codex_launcher") as opened,
                self.assertRaisesRegex(ProcessError, message),
            ):
                process_module._spawn_contained(
                    "/usr/local/bin/codex",
                    ("exec", "prompt"),
                    self.root,
                    MagicMock(),
                    MagicMock(),
                    environment,
                    MagicMock(),
                    self.root,
                    identity,
                )
            popen.assert_not_called()
            opened.assert_not_called()
            runtime.close.assert_called()
            runtime.close.reset_mock()

    def test_payload_sentinel_digest_requires_one_exact_digest_line(self) -> None:
        sentinel = self.root / "sentinel"
        digest = "b" * 64
        sentinel.write_text(f"1 /cgroup\npayload-sha256 {digest}\n")
        self.assertEqual(process_module._payload_sentinel_digest(sentinel), digest)
        for content, message in (
            ("1 /cgroup\n", "lacks a payload digest"),
            (f"1 /cgroup\npayload-sha256 {digest}\nextra\n", "lacks a payload digest"),
            (f"1 /cgroup\npayload-sha512 {digest}\n", "lacks a payload digest"),
            (f"1 /cgroup\npayload-sha256 {digest.upper()}\n", "malformed"),
            ("1 /cgroup\npayload-sha256 unbound\n", "malformed"),
        ):
            with self.subTest(content=content):
                sentinel.write_text(content)
                with self.assertRaisesRegex(ProcessError, message):
                    process_module._payload_sentinel_digest(sentinel)
        with self.assertRaisesRegex(ProcessError, "unreadable"):
            process_module._payload_sentinel_digest(self.root / "missing")

    def test_wrapper_binds_payload_descriptor_to_digest(self) -> None:
        wrapper = _load_payload_wrapper()
        content = b"payload\n" * 300000
        payload = self._trusted_file("payload/codex", content)
        digest = hashlib.sha256(content).hexdigest()
        for expected in (digest, "unbound"):
            with self.subTest(expected=expected):
                descriptor, value = wrapper._bind_payload(str(payload), expected)
                try:
                    self.assertEqual(value, digest)
                    self.assertTrue(os.get_inheritable(descriptor))
                    self.assertEqual(os.fstat(descriptor).st_ino, payload.stat().st_ino)
                finally:
                    os.close(descriptor)
        before = _open_descriptor_count()
        link = payload.with_name("link")
        link.symlink_to(payload)
        writable = self._trusted_file("payload/writable", content, mode=0o775)
        for path, expected, message in (
            (payload, "0" * 64, "differs from its reviewed digest"),
            (payload, "A" * 64, "malformed"),
            (payload, "", "malformed"),
            (link, digest, "cannot be bound"),
            (writable, digest, "differs from its reviewed digest"),
            (payload.with_name("missing"), digest, "cannot be bound"),
        ):
            with (
                self.subTest(path=path.name, expected=expected),
                self.assertRaisesRegex(SystemExit, message),
            ):
                wrapper._bind_payload(str(path), expected)
        self.assertEqual(_open_descriptor_count(), before)

    def test_wrapper_sentinel_round_trips_payload_digest(self) -> None:
        wrapper = _load_payload_wrapper()
        digest = "c" * 64
        sentinel = self.root / "bound.sentinel"
        wrapper._write_sentinel(sentinel, self.root / "cgroup", digest)
        self.assertEqual(process_module._payload_sentinel_digest(sentinel), digest)
        unbound = self.root / "plain.sentinel"
        wrapper._write_sentinel(unbound, self.root / "cgroup", None)
        self.assertEqual(len(unbound.read_text().splitlines()), 1)
        with self.assertRaisesRegex(ProcessError, "lacks a payload digest"):
            process_module._payload_sentinel_digest(unbound)

    def test_wrapper_refuses_empty_upstream_argv(self) -> None:
        wrapper = _load_payload_wrapper()
        diagnostic = "payload wrapper received no upstream executable"
        self.assertEqual(
            wrapper._upstream_command(["payload_wrapper.py", "/bin/true", "-x"]),
            ["/bin/true", "-x"],
        )
        for argv in (["payload_wrapper.py"], ["payload_wrapper.py", ""], []):
            with self.subTest(argv=argv), self.assertRaises(SystemExit) as raised:
                wrapper._upstream_command(argv)
            self.assertEqual(raised.exception.code, diagnostic)
        cgroup_root = self.root / "cgroup"
        (cgroup_root / "payload").mkdir(parents=True)
        procs = cgroup_root / "payload" / "cgroup.procs"
        procs.write_text("")
        sentinel = self.root / "wrapper.sentinel"
        environment = {
            wrapper._CGROUP_ENV: str(cgroup_root),
            wrapper._SENTINEL_ENV: str(sentinel),
            wrapper._STATUS_ENV: str(self.root / "wrapper.status"),
            wrapper._SUPERVISOR_ENV: str(self.root / "supervisor.py"),
            wrapper._PAYLOAD_SHA256_ENV: "d" * 64,
        }
        with (
            patch.dict(os.environ, environment),
            patch.object(wrapper.sys, "argv", ["payload_wrapper.py"]),
            self.assertRaises(SystemExit) as raised,
        ):
            wrapper.main()
        self.assertEqual(raised.exception.code, diagnostic)
        # The refusal precedes cgroup admission, the sentinel and payload binding.
        self.assertEqual(procs.read_text(), "")
        self.assertFalse(os.path.lexists(sentinel))

    def _run_with_fake_clock(
        self,
        timeout_seconds: float,
        startup_timeout_seconds: float | None,
        readiness: Callable[[_FakeClock], dict[str, object] | None] | None,
    ) -> tuple[dict[str, object], list[object], list[float], ProcessError | None]:
        clock = _FakeClock(1000.0)
        waits: list[object] = []
        readiness_deadlines: list[float] = []
        process = MagicMock(pid=os.getpid())
        process.poll.return_value = None

        def wait(timeout: float) -> int:
            waits.append(timeout)
            if len(waits) == 1 and timeout != 5:
                raise subprocess.TimeoutExpired("payload", timeout)
            return -9

        process.wait.side_effect = wait
        artifact_dir = self.root / f"run-{uuid.uuid4().hex}"

        def spawn(*_args: object) -> tuple[object, ...]:
            # Launch-time identity checks and wrapper hashing take time; none of
            # it may be charged before the documented deadline origin.
            clock.now += 100
            return (
                process,
                artifact_dir / "missing.sentinel",
                artifact_dir / "missing.status",
                CONTAINMENT,
                MagicMock(),
            )

        def wait_for_readiness(
            _process: object, _path: Path, deadline: float
        ) -> dict[str, object] | None:
            readiness_deadlines.append(deadline)
            return None if readiness is None else readiness(clock)

        cgroup = MagicMock(path=self.root / "cgroup")
        cgroup.terminate_and_remove.return_value = ()
        readiness_path = (
            None
            if startup_timeout_seconds is None
            else self.root / f"ready-{uuid.uuid4().hex}"
        )
        error: ProcessError | None = None
        with (
            patch.object(process_module, "time", clock),
            patch.object(process_module, "_ProcessCgroup", return_value=cgroup),
            patch.object(process_module, "_spawn_contained", side_effect=spawn),
            patch.object(
                process_module,
                "_wait_for_readiness",
                side_effect=wait_for_readiness,
            ),
        ):
            try:
                run_process(
                    "/usr/local/bin/fbpython",
                    ("-c", "pass"),
                    self.root,
                    artifact_dir,
                    timeout_seconds,
                    readiness_path=readiness_path,
                    startup_timeout_seconds=(
                        10
                        if startup_timeout_seconds is None
                        else startup_timeout_seconds
                    ),
                )
            except ProcessError as raised:
                error = raised
        metadata = json.loads((artifact_dir / "metadata.json").read_text())
        return metadata, waits, readiness_deadlines, error

    def test_execution_deadline_starts_when_launcher_is_spawned(self) -> None:
        metadata, waits, deadlines, error = self._run_with_fake_clock(7, None, None)
        self.assertIsNone(error)
        self.assertEqual(waits, [7, 5])
        self.assertEqual(deadlines, [])
        self.assertTrue(metadata["timed_out"])
        self.assertEqual(metadata["deadline_origin"], "payload-launcher-spawned")
        self.assertIsNone(metadata["startup_timeout_seconds"])
        self.assertEqual(metadata["timeout_seconds"], 7)

    def test_readiness_is_charged_against_one_execution_deadline(self) -> None:
        def ready_after_startup_work(clock: _FakeClock) -> dict[str, object]:
            clock.now += 2.5
            return {"kind": "file"}

        metadata, waits, deadlines, error = self._run_with_fake_clock(
            7, 3, ready_after_startup_work
        )
        self.assertIsNone(error)
        self.assertEqual(deadlines, [1103])
        self.assertEqual(waits, [4.5, 5])
        self.assertEqual(metadata["startup_timeout_seconds"], 3)
        self.assertEqual(metadata["deadline_origin"], "payload-launcher-spawned")

    def test_startup_deadline_shorter_than_execution_is_a_readiness_error(
        self,
    ) -> None:
        metadata, waits, deadlines, error = self._run_with_fake_clock(7, 3, None)
        self.assertIsNotNone(error)
        self.assertRegex(str(error), "readiness timed out")
        self.assertEqual(deadlines, [1103])
        self.assertEqual(waits, [5])
        self.assertEqual(metadata["state"], "running")

    def test_execution_deadline_wins_when_startup_is_not_shorter(self) -> None:
        for startup in (7, 9):
            with self.subTest(startup=startup):
                metadata, waits, deadlines, error = self._run_with_fake_clock(
                    7, startup, None
                )
                self.assertIsNone(error)
                self.assertEqual(deadlines, [1107])
                self.assertEqual(waits, [5])
                self.assertTrue(metadata["timed_out"])
                self.assertEqual(metadata["startup_timeout_seconds"], startup)

    def _run_bound_codex_version(self, binding: str, recorded: str) -> Path:
        artifact_dir = self.root / f"codex-{uuid.uuid4().hex}"
        process = MagicMock(pid=os.getpid())
        process.poll.return_value = 0
        process.wait.return_value = 0

        def spawn(*_args: object) -> tuple[object, ...]:
            sentinel = artifact_dir / "payload.sentinel"
            status = artifact_dir / "payload.status"
            sentinel.write_text(f"1 /cgroup\npayload-sha256 {recorded}\n")
            status.write_text("0 0 123\n")
            os.chmod(sentinel, 0o600)
            os.chmod(status, 0o600)
            return (process, sentinel, status, CONTAINMENT, MagicMock())

        cgroup = MagicMock(path=self.root / "cgroup")
        cgroup.terminate_and_remove.return_value = ()
        with (
            patch.object(process_module, "_ProcessCgroup", return_value=cgroup),
            patch.object(process_module, "_spawn_contained", side_effect=spawn),
            patch.object(
                process_module, "_codex_payload_binding", return_value=binding
            ),
        ):
            run_process(
                "/usr/local/bin/codex", ("--version",), self.root, artifact_dir, 10
            )
        return artifact_dir

    def test_run_process_enforces_wrapper_reported_payload_digest(self) -> None:
        artifact_dir = self._run_bound_codex_version("unbound", "d" * 64)
        metadata = json.loads((artifact_dir / "metadata.json").read_text())
        self.assertEqual(metadata["payload_binding"], "unbound")
        self.assertEqual(metadata["payload_sha256"], "d" * 64)
        artifact_dir = self._run_bound_codex_version("d" * 64, "d" * 64)
        metadata = json.loads((artifact_dir / "metadata.json").read_text())
        self.assertEqual(metadata["payload_binding"], "d" * 64)
        with self.assertRaisesRegex(ProcessError, "differs from its reviewed digest"):
            self._run_bound_codex_version("e" * 64, "d" * 64)


def _valid_report(case_id: str, artifact_dir: Path) -> dict[str, object]:
    return {
        "schema_version": 1,
        "qualification": "candidate-only",
        "case_id": case_id,
        "reproduction": {"pass_reproduced": True, "fail_reproduced": True},
        "hypotheses": [
            {
                "rank": 1,
                "claim": "race",
                "supporting": "trace",
                "contradicting": "none",
                "next_experiment": "instrument",
            },
            {
                "rank": 2,
                "claim": "lifetime",
                "supporting": "code",
                "contradicting": "schedule",
                "next_experiment": "assert",
            },
        ],
        "selected_hypothesis_rank": 1,
        "root_cause": "An invariant was violated before the fatal symptom.",
        "patch_path": str(artifact_dir / "patch.diff"),
        "original_fail_now_passes": True,
        "original_pass_still_passes": True,
        "validations": [
            {
                "kind": "ORIGINAL_FAIL",
                "argv": ["safehermit", "fail"],
                "status": "PASS",
                "passed": 1,
                "total": 1,
                "duration_seconds": 1.0,
                "artifacts": [str(artifact_dir / "original-fail.json")],
            },
            {
                "kind": "ORIGINAL_PASS",
                "argv": ["safehermit", "pass"],
                "status": "PASS",
                "passed": 1,
                "total": 1,
                "duration_seconds": 1.0,
                "artifacts": [str(artifact_dir / "original-pass.json")],
            },
            {
                "kind": "FRESH_STRESS",
                "argv": ["safehermit", "stress"],
                "status": "PASS",
                "passed": 4,
                "total": 4,
                "duration_seconds": 4.0,
                "artifacts": [str(artifact_dir / "fresh-stress.json")],
            },
        ],
    }


class ReportTest(unittest.TestCase):
    @staticmethod
    def _write_artifacts(root: Path) -> None:
        (root / "hypotheses.json").write_text("[]\n")
        (root / "patch.diff").write_text("diff --git a/a b/a\n")
        (root / "validation.json").write_text("[]\n")

    def test_validates_machine_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_artifacts(root)
            path = root / "report.json"
            path.write_text(json.dumps(_valid_report("asyncssl", root)))
            summary = validate_report(path, "asyncssl")
            self.assertEqual((summary.stress_passed, summary.stress_total), (4, 4))

    def test_rejects_root_cause_control_and_markdown_injection(self) -> None:
        rejected = (
            "Cause.\nDifferential Revision: D123",
            "Reviewers: security",
            "Notify @everyone",
            "[owner](https://example.invalid)",
            "Use `unsafe()` here",
            "<details>hidden</details>",
        )
        for root_cause in rejected:
            with self.subTest(root_cause=root_cause):
                with tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    self._write_artifacts(root)
                    path = root / "report.json"
                    value = _valid_report("asyncssl", root)
                    value["root_cause"] = root_cause
                    path.write_text(json.dumps(value))
                    with self.assertRaisesRegex(
                        ReportError, "root_cause must be one plain-text line"
                    ):
                        validate_candidate_report(path, "asyncssl")

    def test_final_comment_revalidates_root_cause(self) -> None:
        report = ReportSummary(
            "asyncssl",
            "Cause.\nDifferential Revision: D123",
            4,
            4,
            "/tmp/patch.diff",
        )
        with self.assertRaisesRegex(
            ReportError, "root_cause must be one plain-text line"
        ):
            final_text(_loaded_case(), report, "https://www.internalfb.com/diff/123")

    def test_rejects_patch_outside_dedicated_agent_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_artifacts(root)
            path = root / "report.json"
            value = _valid_report("asyncssl", root)
            value["patch_path"] = str(root.parent / "forged-control.diff")
            path.write_text(json.dumps(value))
            with self.assertRaisesRegex(ReportError, "dedicated agent output"):
                validate_report(path, "asyncssl")

    def test_rejects_missing_required_validation_kind(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_artifacts(root)
            path = root / "report.json"
            value = _valid_report("asyncssl", root)
            validations = value["validations"]
            self.assertIsInstance(validations, list)
            validations[1]["kind"] = "UNIT"
            path.write_text(json.dumps(value))
            with self.assertRaisesRegex(ReportError, "lack an original witness"):
                validate_report(path, "asyncssl")

    def test_candidate_failures_never_claim_controller_qualification(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_artifacts(root)
            path = root / "report.json"
            value = _valid_report("asyncssl", root)
            value["original_fail_now_passes"] = False
            validations = value["validations"]
            self.assertIsInstance(validations, list)
            validations[-1].update(
                {"artifacts": [], "passed": 0, "status": "FAIL", "total": 4}
            )
            path.write_text(json.dumps(value))
            candidate = validate_candidate_report(path, "asyncssl")
            self.assertEqual(candidate.patch_path, str(root / "patch.diff"))
            with self.assertRaisesRegex(ReportError, "failing seed must pass"):
                validate_report(path, "asyncssl")

    def test_qualified_report_is_bound_to_controller_and_candidate_seals(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            case = Path(temporary) / "case"
            output = case / "agent-output"
            output.mkdir(parents=True)
            self._write_artifacts(output)
            report = output / "report.json"
            report.write_text(json.dumps(_valid_report("asyncssl", output)))
            control = case / "control"
            attempt = control / "candidate-attempts" / "attempt-001"
            attempt.mkdir(parents=True)
            candidate_seal = attempt / "candidate-seal.json"
            seal_candidate_report(report, candidate_seal, "asyncssl")
            handoff = attempt / "handoff.json"
            applied_diff = attempt / "applied-diff" / "stdout.txt"
            applied_diff.parent.mkdir()
            applied_diff.write_text("diff\n")
            apply_process = attempt / "apply" / "metadata.json"
            apply_process.parent.mkdir()
            apply_process.write_text("{}\n")
            approved_patch = attempt / "approved-patch.diff"
            approved_patch.write_bytes((output / "patch.diff").read_bytes())
            candidate_approval = attempt / "candidate-approval.json"
            candidate_approval.write_text("{}\n")
            handoff_value: dict[str, object] = {
                "applied_diff": regular_file_record(applied_diff),
                "apply_process": regular_file_record(apply_process),
                "approved_patch": regular_file_record(approved_patch),
                "candidate_patch": regular_file_record(output / "patch.diff"),
                "changed_paths": ["fbcode/product/fix.cpp"],
            }
            write_metadata(handoff, handoff_value)
            transaction = attempt / "transaction.json"
            write_metadata(
                transaction,
                {
                    **handoff_value,
                    "candidate_approval": regular_file_record(candidate_approval),
                    "expected_paths": ["fbcode/product/fix.cpp"],
                    "handoff": regular_file_record(handoff),
                    "phase": "validating",
                },
            )
            active = control / "active-candidate.json"
            active.write_text("{}\n")
            validation = control / "validation.json"
            validation.write_text("{}\n")
            validation_seal = control / "validation-seal.json"
            validation_seal.write_text("{}\n")
            qualified = control / "qualified-report.json"
            write_metadata(
                qualified,
                {
                    "active_candidate": regular_file_record(active),
                    "agent_validation_qualifies": False,
                    "candidate_report": regular_file_record(report),
                    "candidate_approval": regular_file_record(candidate_approval),
                    "candidate_seal": regular_file_record(candidate_seal),
                    "candidate_transaction": regular_file_record(transaction),
                    "case_id": "asyncssl",
                    "changed_paths": ["fbcode/product/fix.cpp"],
                    "controller_validation": regular_file_record(validation),
                    "handoff": regular_file_record(handoff),
                    "handoff_applied_diff": handoff_value["applied_diff"],
                    "handoff_apply_process": handoff_value["apply_process"],
                    "handoff_approved_patch": handoff_value["approved_patch"],
                    "handoff_candidate_patch": handoff_value["candidate_patch"],
                    "patch_path": str(output / "patch.diff"),
                    "qualified": True,
                    "root_cause": "An invariant was violated before the fatal symptom.",
                    "schema_version": 1,
                    "stress_passed": 4,
                    "stress_total": 4,
                    "validation_seal": regular_file_record(validation_seal),
                },
            )
            summary = validate_qualified_report(qualified, "asyncssl")
            self.assertEqual(summary.stress_total, 4)
            qualified_value = json.loads(qualified.read_text())
            for key, evidence in (
                ("handoff_applied_diff", applied_diff),
                ("handoff_apply_process", apply_process),
                ("handoff_approved_patch", approved_patch),
                ("handoff_candidate_patch", output / "patch.diff"),
            ):
                with self.subTest(record=key, mutation="deleted"):
                    original = evidence.read_bytes()
                    mode = stat.S_IMODE(evidence.stat().st_mode)
                    evidence.unlink()
                    with self.assertRaisesRegex(ReportError, "differs from its seal"):
                        validate_qualified_report(qualified, "asyncssl")
                    evidence.write_bytes(original)
                    evidence.chmod(mode)
                with self.subTest(record=key, mutation="type"):
                    evidence.unlink()
                    evidence.mkdir()
                    with self.assertRaisesRegex(ReportError, "differs from its seal"):
                        validate_qualified_report(qualified, "asyncssl")
                    evidence.rmdir()
                    evidence.write_bytes(original)
                    evidence.chmod(mode)
                with self.subTest(record=key, mutation="mode"):
                    evidence.chmod(mode ^ stat.S_IXUSR)
                    with self.assertRaisesRegex(ReportError, "differs from its seal"):
                        validate_qualified_report(qualified, "asyncssl")
                    evidence.chmod(mode)
                with self.subTest(record=key, mutation="size"):
                    evidence.write_bytes(original + b"x")
                    with self.assertRaisesRegex(ReportError, "differs from its seal"):
                        validate_qualified_report(qualified, "asyncssl")
                    evidence.write_bytes(original)
                    evidence.chmod(mode)
                with self.subTest(record=key, mutation="content"):
                    replacement = bytes([original[0] ^ 1]) + original[1:]
                    evidence.write_bytes(replacement)
                    with self.assertRaisesRegex(ReportError, "differs from its seal"):
                        validate_qualified_report(qualified, "asyncssl")
                    evidence.write_bytes(original)
                    evidence.chmod(mode)
                with self.subTest(record=key, mutation="path"):
                    substitute = evidence.with_name(evidence.name + ".substitute")
                    substitute.write_bytes(original)
                    substitute.chmod(mode)
                    substituted_value = json.loads(json.dumps(qualified_value))
                    record = substituted_value[key]
                    self.assertIsInstance(record, dict)
                    record["path"] = str(substitute)
                    write_metadata(qualified, substituted_value)
                    with self.assertRaisesRegex(ReportError, "path is invalid"):
                        validate_qualified_report(qualified, "asyncssl")
                    write_metadata(qualified, qualified_value)
            report.write_text("{}\n")
            with self.assertRaisesRegex(ReportError, "differs from its seal"):
                validate_qualified_report(qualified, "asyncssl")


class DraftTest(unittest.TestCase):
    _EXPECTED_MESSAGE = "fixed controller message"

    @staticmethod
    def _result(root: Path, name: str, text: str) -> ProcessResult:
        stdout = root / f"{name}.stdout"
        stderr = root / f"{name}.stderr"
        stdout.write_text(text)
        stderr.write_text("")
        return ProcessResult((name,), 0, 0.1, False, stdout, stderr)

    def test_accepts_only_draft_hold_without_reviewers(self) -> None:
        commands = [
            [
                "/usr/local/bin/jf",
                "--json",
                "submit",
                "--draft",
                "--no-request-review",
                "--verbose",
            ],
        ]
        validate_draft_plan(commands, [])
        url = validate_draft_readback(
            {
                "number": "D123",
                "status": "Unpublished",
                "reviewers": [],
                "tags": [],
                "publish_when_ready": False,
                "is_landing": False,
                "land_job_status": "NO_LAND_RUNNING",
            }
        )
        self.assertEqual(url, "https://www.internalfb.com/diff/123")

    def test_rejects_reviewer_and_publish_actions(self) -> None:
        with self.assertRaisesRegex(DraftError, "reviewers"):
            validate_draft_plan([], ["reviewer"])
        with self.assertRaisesRegex(DraftError, "forbidden"):
            validate_draft_plan([["/usr/local/bin/jf", "publish"]], [])
        with self.assertRaisesRegex(DraftError, "has reviewers"):
            validate_draft_readback(
                {
                    "number": "D123",
                    "status": "Unpublished",
                    "reviewers": ["fallback-reviewer"],
                    "tags": [],
                    "publish_when_ready": False,
                }
            )
        with self.assertRaisesRegex(DraftError, "publish or autoland tag"):
            validate_draft_readback(
                {
                    "number": "D123",
                    "status": "Unpublished",
                    "reviewers": [],
                    "tags": ["autoland"],
                    "publish_when_ready": False,
                }
            )

    def test_create_draft_submits_then_reads_back_safety_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            submit = ProcessResult(
                ("jf",), 0, 0.1, False, root / "submit", root / "submit-err"
            )
            submit.stdout_path.write_text(
                '{"type":"step","data":{"message":"submit"}}\n'
                '{"type":"sublist","data":[{"item":'
                '"created: https://www.internalfb.com/diff/D123"}]}\n'
            )
            describe = ProcessResult(
                ("meta",), 0, 0.1, False, root / "readback", root / "readback-err"
            )
            files = ProcessResult(
                ("meta",), 0, 0.1, False, root / "files", root / "files-err"
            )
            readback = DraftReadback(
                "https://www.internalfb.com/diff/123",
                "D123",
                "commit",
                describe,
                files,
            )
            with (
                patch(
                    "testinfra.tfa.drafts._verify_local_commit",
                    return_value="commit",
                ) as verify,
                patch(
                    "testinfra.tfa.drafts._sl_value",
                    side_effect=(self._EXPECTED_MESSAGE, ""),
                ),
                patch("testinfra.tfa.drafts.run_process", return_value=submit) as run,
                patch(
                    "testinfra.tfa.drafts.readback_draft", return_value=readback
                ) as read,
            ):
                mutation = create_draft(
                    root,
                    root / "artifacts",
                    "base",
                    ("file.cpp",),
                    [{"kind": "file", "path": "file.cpp", "sha256": "hash"}],
                    self._EXPECTED_MESSAGE,
                )
            self.assertEqual(mutation.url, "https://www.internalfb.com/diff/123")
            verify.assert_called_once()
            run.assert_called_once()
            read.assert_called_once()

    def test_create_draft_recovers_successful_submit_before_readback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            submit_dir = root / "artifacts" / "submit"
            submit_dir.mkdir(parents=True)
            submit_argv = [
                "/usr/local/bin/jf",
                "--json",
                "submit",
                "--draft",
                "--no-request-review",
                "--verbose",
            ]
            (submit_dir / "metadata.json").write_text(
                json.dumps(
                    {
                        "argv": submit_argv,
                        "duration_seconds": 1.0,
                        "returncode": 0,
                        "state": "completed",
                        "timed_out": False,
                    }
                )
            )
            (submit_dir / "stdout.txt").write_text(
                '{"type":"sublist","data":[{"item":'
                '"created: https://www.internalfb.com/diff/D123"}]}\n'
            )
            (submit_dir / "stderr.txt").write_text("")
            payload_sentinel = submit_dir / "payload-recovered.sentinel"
            payload_status = submit_dir / "payload-recovered.status"
            payload_sentinel.write_text("123 /sys/fs/cgroup/tfa-test\n")
            payload_status.write_text("0 0 456\n")
            payload_sentinel.chmod(0o600)
            payload_status.chmod(0o600)
            write_metadata(
                submit_dir / "metadata.json",
                {
                    "argv": submit_argv,
                    "containment": CONTAINMENT,
                    "containment_empty": True,
                    "descendants_terminated": 0,
                    "duration_seconds": 1.0,
                    "cwd": str(root),
                    "outer_filesystem_isolation": "none",
                    "outer_network_isolation": "none",
                    "payload_isolation": "trusted-executable",
                    "process_group": 123,
                    "payload_sentinel": regular_file_record(payload_sentinel),
                    "payload_status": regular_file_record(payload_status),
                    "payload_runtime": payload_runtime_identity(),
                    "payload_pid": 456,
                    "returncode": 0,
                    "state": "completed",
                    "stderr": regular_file_record(submit_dir / "stderr.txt"),
                    "stdout": regular_file_record(submit_dir / "stdout.txt"),
                    "timed_out": False,
                },
            )
            describe = ProcessResult(
                ("meta",),
                0,
                0.1,
                False,
                root / "readback",
                root / "readback-err",
            )
            files = ProcessResult(
                ("meta",), 0, 0.1, False, root / "files", root / "files-err"
            )
            readback = DraftReadback(
                "https://www.internalfb.com/diff/123",
                "D123",
                "commit",
                describe,
                files,
            )
            with (
                patch(
                    "testinfra.tfa.drafts._verify_local_commit",
                    return_value="commit",
                ) as verify,
                patch(
                    "testinfra.tfa.drafts._sl_value",
                    side_effect=(self._EXPECTED_MESSAGE, ""),
                ),
                patch("testinfra.tfa.drafts.run_process") as run,
                patch(
                    "testinfra.tfa.drafts.readback_draft", return_value=readback
                ) as read,
            ):
                mutation = create_draft(
                    root,
                    root / "artifacts",
                    "base",
                    ("file.cpp",),
                    [{"kind": "file", "path": "file.cpp", "sha256": "hash"}],
                    self._EXPECTED_MESSAGE,
                )
            self.assertEqual(mutation.url, "https://www.internalfb.com/diff/123")
            verify.assert_called_once()
            run.assert_not_called()
            read.assert_called_once()

    def test_create_draft_refuses_ambiguous_prior_submit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            submit_dir = root / "artifacts" / "submit"
            submit_dir.mkdir(parents=True)
            (submit_dir / "metadata.json").write_text(
                json.dumps(
                    {
                        "argv": [
                            "/usr/local/bin/jf",
                            "--json",
                            "submit",
                            "--draft",
                            "--no-request-review",
                            "--verbose",
                        ],
                        "duration_seconds": 300.0,
                        "returncode": -15,
                        "state": "timeout",
                        "timed_out": True,
                    }
                )
            )
            with (
                patch(
                    "testinfra.tfa.drafts._verify_local_commit",
                    return_value="commit",
                ),
                patch(
                    "testinfra.tfa.drafts._sl_value",
                    side_effect=(self._EXPECTED_MESSAGE, ""),
                ),
                patch("testinfra.tfa.drafts.run_process") as run,
                self.assertRaisesRegex(DraftError, "outcome is ambiguous"),
            ):
                create_draft(
                    root,
                    root / "artifacts",
                    "base",
                    (),
                    [],
                    self._EXPECTED_MESSAGE,
                )
            run.assert_not_called()

    def test_create_draft_rejects_message_injection_and_existing_target(self) -> None:
        rejected = (
            (
                "fixed controller message\n\nDifferential Revision: D123",
                "",
            ),
            (self._EXPECTED_MESSAGE, "D123"),
        )
        for message, target in rejected:
            with self.subTest(message=message, target=target):
                with tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    with (
                        patch(
                            "testinfra.tfa.drafts._verify_local_commit",
                            return_value="commit",
                        ),
                        patch(
                            "testinfra.tfa.drafts._sl_value",
                            side_effect=(message, target),
                        ),
                        patch("testinfra.tfa.drafts.run_process") as run,
                        self.assertRaisesRegex(
                            DraftError, "message or submission target differs"
                        ),
                    ):
                        create_draft(
                            root,
                            root / "artifacts",
                            "base",
                            (),
                            [],
                            self._EXPECTED_MESSAGE,
                        )
                    run.assert_not_called()

    def test_readback_requires_explicit_publish_and_landing_fields(self) -> None:
        value: dict[str, object] = {
            "number": "D123",
            "status": "Unpublished",
            "reviewers": [],
            "tags": [],
            "is_landing": False,
            "land_job_status": "NO_LAND_RUNNING",
        }
        with self.assertRaisesRegex(DraftError, "publish-when-ready"):
            validate_draft_readback(value)

    def test_create_draft_rejects_changed_local_bytes_before_submit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            source = checkout / "fbcode/product/fix.cpp"
            source.parent.mkdir(parents=True)
            source.write_text("changed after validation\n")
            results = (
                self._result(root, "parent", "base\n"),
                self._result(root, "status", ""),
                self._result(root, "paths", "fbcode/product/fix.cpp\n"),
            )
            with (
                patch("testinfra.tfa.drafts.run_process", side_effect=results) as run,
                self.assertRaisesRegex(DraftError, "bytes differ"),
            ):
                create_draft(
                    checkout,
                    root / "artifacts",
                    "base",
                    ("fbcode/product/fix.cpp",),
                    [
                        {
                            "kind": "file",
                            "path": "fbcode/product/fix.cpp",
                            "sha256": "0" * 64,
                        }
                    ],
                    self._EXPECTED_MESSAGE,
                )
            self.assertEqual(run.call_count, 3)
            self.assertNotIn(
                "/usr/local/bin/jf", [call.args[0] for call in run.call_args_list]
            )

    def test_create_draft_rejects_executable_bit_drift_before_submit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            source = checkout / "fbcode/product/fix.cpp"
            source.parent.mkdir(parents=True)
            source.write_text("fixed\n")
            results = (
                self._result(root, "parent", "base\n"),
                self._result(root, "status", ""),
                self._result(root, "paths", "fbcode/product/fix.cpp\n"),
            )
            with (
                patch("testinfra.tfa.drafts.run_process", side_effect=results),
                self.assertRaisesRegex(DraftError, "mode differs"),
            ):
                create_draft(
                    checkout,
                    root / "artifacts",
                    "base",
                    ("fbcode/product/fix.cpp",),
                    [
                        {
                            "kind": "file",
                            "mode": 0o755,
                            "path": "fbcode/product/fix.cpp",
                            "sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                        }
                    ],
                    self._EXPECTED_MESSAGE,
                )

    def test_create_draft_rejects_symlink_substituted_for_validated_file(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            source = checkout / "fbcode/product/fix.cpp"
            source.parent.mkdir(parents=True)
            target = root / "target.cpp"
            target.write_text("fixed\n")
            source.symlink_to(target)
            results = (
                self._result(root, "parent", "base\n"),
                self._result(root, "status", ""),
                self._result(root, "paths", "fbcode/product/fix.cpp\n"),
            )
            with (
                patch("testinfra.tfa.drafts.run_process", side_effect=results),
                self.assertRaisesRegex(DraftError, "bytes differ"),
            ):
                create_draft(
                    checkout,
                    root / "artifacts",
                    "base",
                    ("fbcode/product/fix.cpp",),
                    [
                        {
                            "kind": "file",
                            "mode": source.lstat().st_mode & 0o7777,
                            "path": "fbcode/product/fix.cpp",
                            "sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
                        }
                    ],
                    self._EXPECTED_MESSAGE,
                )

    def test_readback_rejects_unvalidated_remote_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            source = checkout / "fbcode/product/fix.cpp"
            source.parent.mkdir(parents=True)
            source.write_text("fixed\n")
            sha256 = hashlib.sha256(source.read_bytes()).hexdigest()
            annotation = json.dumps(
                {"control": "AUTONOMOUS", "mutation_risk_level": "NONE"}
            )
            results = (
                self._result(root, "parent", "base\n"),
                self._result(root, "status", ""),
                self._result(root, "paths", "fbcode/product/fix.cpp\n"),
                self._result(root, "commit", "commit\n"),
                self._result(root, "describe-preflight", annotation),
                self._result(root, "files-preflight", annotation),
                self._result(
                    root,
                    "describe",
                    json.dumps(
                        {
                            "commit_hash": "commit",
                            "is_landing": False,
                            "land_job_status": "NO_LAND_RUNNING",
                            "number": "D123",
                            "publish_when_ready": "N/A",
                            "reviewers": "",
                            "status": "Unpublished",
                            "tags": "",
                        }
                    ),
                ),
                self._result(
                    root,
                    "files",
                    json.dumps(
                        [
                            {"path": "fbcode/product/fix.cpp"},
                            {"path": "xplat/product/fix.cpp"},
                        ]
                    ),
                ),
            )
            with (
                patch("testinfra.tfa.drafts.run_process", side_effect=results),
                self.assertRaisesRegex(DraftError, "paths differ"),
            ):
                readback_draft(
                    checkout,
                    root / "artifacts",
                    "D123",
                    "base",
                    ("fbcode/product/fix.cpp",),
                    [
                        {
                            "kind": "file",
                            "mode": source.lstat().st_mode & 0o7777,
                            "path": "fbcode/product/fix.cpp",
                            "sha256": sha256,
                        }
                    ],
                )


class DryRunTest(unittest.TestCase):
    def test_three_case_end_to_end_dry_run(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            fake = ProcessResult(
                ("meta",), 0, 0.1, False, root / "stdout", root / "stderr"
            )
            fake.stdout_path.write_text("[]\n")
            with (
                patch("testinfra.tfa.orchestrator.list_comments", return_value=fake),
                patch("testinfra.tfa.orchestrator.dry_run_payload", return_value=fake),
            ):
                output = exercise_dry_run(
                    load_manifest(manifest_path), root / "artifacts", root
                )
            report = json.loads(output.read_text())
            self.assertEqual(len(report["cases"]), 3)
            for case in report["cases"]:
                self.assertEqual(
                    case["stages"],
                    [Stage.VALIDATED_REPRO.value, Stage.INITIAL_COMMENT.value],
                )
                case_dir = root / "artifacts" / case["case_id"]
                self.assertFalse((case_dir / "hypotheses.json").exists())
                self.assertFalse((case_dir / "patch.diff").exists())
                self.assertFalse((case_dir / "validation.json").exists())
                self.assertFalse((case_dir / "diff-url.json").exists())

    def test_comment_list_failure_prevents_payload(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            failed = ProcessResult(
                ("meta",), 1, 0.1, False, root / "stdout", root / "stderr"
            )
            failed.stdout_path.write_text("{}\n")
            with (
                patch("testinfra.tfa.orchestrator.list_comments", return_value=failed),
                patch("testinfra.tfa.orchestrator.dry_run_payload") as payload,
                self.assertRaisesRegex(RuntimeError, "comment list failed"),
            ):
                exercise_dry_run(load_manifest(manifest_path), root / "artifacts", root)
            payload.assert_not_called()

    def test_comment_payload_failure_never_reports_dry_run_complete(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            listed = ProcessResult(
                ("meta",), 0, 0.1, False, root / "list.stdout", root / "list.stderr"
            )
            listed.stdout_path.write_text("[]\n")
            listed.stderr_path.write_text("")
            failed = ProcessResult(
                ("meta",), 17, 0.1, False, root / "dry.stdout", root / "dry.stderr"
            )
            failed.stdout_path.write_text("")
            failed.stderr_path.write_text("refused\n")
            artifact_root = root / "artifacts"
            with (
                patch(
                    "testinfra.tfa.orchestrator.list_comments",
                    return_value=listed,
                ),
                patch(
                    "testinfra.tfa.orchestrator.dry_run_payload",
                    return_value=failed,
                ),
                self.assertRaisesRegex(RuntimeError, "dry-run payload failed"),
            ):
                exercise_dry_run(load_manifest(manifest_path), artifact_root, root)
            self.assertFalse((artifact_root / "experiment-report.json").exists())
            preflight = json.loads(
                (artifact_root / "asyncssl" / "comment-preflight.json").read_text()
            )
            self.assertEqual(preflight["returncode"], 17)
            self.assertEqual(preflight["state"], "failed")
            stages = Ledger(artifact_root / "asyncssl" / "ledger.jsonl").read()
            self.assertEqual([entry.stage for entry in stages], [Stage.VALIDATED_REPRO])


class InvestigationRecoveryTest(unittest.TestCase):
    @_serialized_unit_working_directory
    def test_run_agent_delegates_candidate_to_controller_handoff(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            manifest = load_manifest(manifest_path)
            case = manifest.case("asyncssl")
            case_dir = root / "artifacts" / "asyncssl"
            control_dir = case_dir / "control"
            ledger = Ledger(control_dir / "ledger.jsonl")
            for stage in (
                Stage.VALIDATED_REPRO,
                Stage.INITIAL_COMMENT,
                Stage.INVESTIGATING,
            ):
                ledger.append(stage, {})
            checkout = root / "checkout"
            checkout.mkdir()
            base_stdout = root / "base-stdout"
            status_stdout = root / "status-stdout"
            stderr = root / "stderr"
            base_stdout.write_text(case.source_revision + "\n")
            status_stdout.write_text("")
            stderr.write_text("")
            candidate = CandidateSummary(
                "asyncssl",
                "The originating invariant was violated.",
                str(case_dir / "agent-output" / "patch.diff"),
            )
            with (
                patch(
                    "testinfra.tfa.orchestrator.run_process",
                    side_effect=(
                        ProcessResult(
                            ("sl", "log"), 0, 0.1, False, base_stdout, stderr
                        ),
                        ProcessResult(
                            ("sl", "status"), 0, 0.1, False, status_stdout, stderr
                        ),
                    ),
                ),
                patch(
                    "testinfra.tfa.orchestrator.diagnose_candidate",
                    return_value=candidate,
                ) as diagnose,
            ):
                run_investigation(
                    manifest,
                    "asyncssl",
                    root / "artifacts",
                    checkout,
                    root / "schema.json",
                    10,
                )
            diagnose.assert_called_once_with(
                case, case_dir, checkout, root / "schema.json", 10
            )
            self.assertEqual(ledger.read()[-1].stage, Stage.INVESTIGATING)

    @_serialized_unit_working_directory
    def test_run_agent_refuses_to_bootstrap_coordinator_stages(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            with (
                patch("testinfra.tfa.orchestrator.run_process") as process,
                self.assertRaisesRegex(RuntimeError, "coordinator preflight"),
            ):
                run_investigation(
                    load_manifest(manifest_path),
                    "asyncssl",
                    root / "artifacts",
                    root,
                    root / "schema.json",
                    10,
                )
            process.assert_not_called()

    @_serialized_unit_working_directory
    def test_run_agent_recovers_controller_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            case_dir = root / "artifacts" / "asyncssl"
            control_dir = case_dir / "control"
            ledger = Ledger(control_dir / "ledger.jsonl")
            for stage in (
                Stage.VALIDATED_REPRO,
                Stage.INITIAL_COMMENT,
                Stage.INVESTIGATING,
                Stage.FIX_DRAFTED,
            ):
                ledger.append(stage, {})
            case = load_manifest(manifest_path).case("asyncssl")
            candidate = CandidateSummary(
                "asyncssl",
                "The originating invariant was violated.",
                str(case_dir / "agent-output" / "patch.diff"),
            )
            base_stdout = root / "base-stdout"
            status_stdout = root / "status-stdout"
            stderr = root / "stderr"
            base_stdout.write_text(case.source_revision + "\n")
            status_stdout.write_text("")
            base_result = ProcessResult(
                ("sl", "log"), 0, 0.1, False, base_stdout, stderr
            )
            status_result = ProcessResult(
                ("sl", "status"), 0, 0.1, False, status_stdout, stderr
            )
            with (
                patch(
                    "testinfra.tfa.orchestrator.run_process",
                    side_effect=(base_result, status_result),
                ) as process,
                patch(
                    "testinfra.tfa.orchestrator.diagnose_candidate",
                    return_value=candidate,
                ) as diagnose,
            ):
                summary = run_investigation(
                    load_manifest(manifest_path),
                    "asyncssl",
                    root / "artifacts",
                    root,
                    root / "schema.json",
                    10,
                )
            self.assertEqual(summary, candidate)
            self.assertEqual(ledger.read()[-1].stage, Stage.FIX_DRAFTED)
            self.assertEqual(process.call_count, 2)
            diagnose.assert_called_once()


class RunCaseTest(unittest.TestCase):
    def test_runtime_staging_byte_budget_refuses_before_copy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.write_bytes(b"four")
            destination = root / "destination"
            destination.symlink_to(source)
            with self.assertRaisesRegex(RuntimeError, "byte budget exceeded"):
                pipeline_module._copy_pinned_runtime_source(source, destination, 3)
            self.assertTrue(destination.is_symlink())
            self.assertEqual(destination.resolve(), source)
            self.assertEqual(
                tuple(sorted(path.name for path in root.iterdir())),
                ("destination", "source"),
            )

    def test_runtime_staging_repeated_crash_exhausts_single_generation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            buck_out = root / "checkout" / "buck-out"
            output = buck_out / "v2" / "art" / "target" / "__test__"
            output.mkdir(parents=True)
            elf = output / "test"
            elf.write_bytes(b"elf")
            tree = output / "__test__shared_libs_symlink_tree"
            tree.mkdir()
            external = root / "external" / "libexternal.so"
            external.parent.mkdir()
            external.write_bytes(b"external")
            (tree / external.name).symlink_to(external)
            evidence = root / "attempt" / "runtime.json"
            real_copy = pipeline_module._copy_pinned_runtime_source

            def fail_external(
                source: Path, destination: Path, max_bytes: int
            ) -> tuple[dict[str, object], dict[str, object]]:
                if source == external:
                    raise RuntimeError("injected crash")
                return real_copy(source, destination, max_bytes)

            with (
                patch(
                    "testinfra.tfa.pipeline._copy_pinned_runtime_source",
                    side_effect=fail_external,
                ),
                self.assertRaisesRegex(RuntimeError, "injected crash"),
            ):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, evidence
                )
            transaction_path = evidence.with_name(
                "runtime-materialization-transaction.json"
            )
            transaction = json.loads(transaction_path.read_text())
            staging = Path(str(transaction["staging"]))
            sentinel = staging / "preserve-me"
            sentinel.write_text("preserve\n")
            runtime_parent = staging.parent
            for _attempt in range(2):
                before = tuple(sorted(path.name for path in runtime_parent.iterdir()))
                with self.assertRaisesRegex(RuntimeError, "requires explicit cleanup"):
                    pipeline_module._materialize_external_runtime_links(
                        elf, buck_out, evidence
                    )
                self.assertEqual(
                    tuple(sorted(path.name for path in runtime_parent.iterdir())),
                    before,
                )
                self.assertEqual(sentinel.read_text(), "preserve\n")
            preserved = json.loads(transaction_path.read_text())["preserved_staging"]
            self.assertEqual([record["path"] for record in preserved], [str(staging)])

    def test_runtime_dependency_closure_rejects_tamper_missing_and_retarget(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            buck_out = root / "checkout" / "buck-out"
            output = buck_out / "v2" / "art" / "target" / "__test__"
            output.mkdir(parents=True)
            elf = output / "test"
            elf.write_bytes(b"elf")
            tree = output / "__test__shared_libs_symlink_tree"
            tree.mkdir()
            internal = buck_out / "v2" / "art" / "libs" / "libinternal.so"
            internal.parent.mkdir(parents=True)
            internal.write_bytes(b"internal")
            external = root / "external" / "libexternal.so"
            external.parent.mkdir()
            external.write_bytes(b"external")
            internal_link = tree / "libinternal.so"
            external_link = tree / "libexternal.so"
            internal_link.symlink_to(internal)
            external_link.symlink_to(external)
            evidence = root / "runtime.json"

            race_source = root / "external" / "librace.so"
            race_source.write_bytes(b"pinned")
            race_link = tree / "librace.so"
            race_link.symlink_to(race_source)
            real_read = os.read
            replaced = False

            def replace_source_after_read(descriptor: int, size: int) -> bytes:
                nonlocal replaced
                data = real_read(descriptor, size)
                if data and not replaced:
                    replaced = True
                    race_source.unlink()
                    race_source.write_bytes(b"replacement")
                return data

            with (
                patch(
                    "testinfra.tfa.pipeline.os.read",
                    side_effect=replace_source_after_read,
                ),
                self.assertRaisesRegex(RuntimeError, "changed during materialization"),
            ):
                pipeline_module._copy_pinned_runtime_source(race_source, race_link)
            self.assertTrue(race_link.is_symlink())
            race_link.unlink()
            race_source.unlink()

            inventory = pipeline_module._materialize_external_runtime_links(
                elf, buck_out, evidence
            )
            closure = inventory["closure"]
            self.assertIsInstance(closure, list)
            self.assertEqual(len(closure), 2)
            runtime_elf_record = inventory["runtime_elf"]
            self.assertIsInstance(runtime_elf_record, dict)
            runtime_elf = Path(str(runtime_elf_record["path"]))
            runtime_tree = Path(str(inventory["tree"]))
            runtime_internal_link = runtime_tree / internal_link.name
            runtime_external_file = runtime_tree / external_link.name
            self.assertTrue(internal_link.is_symlink())
            self.assertTrue(external_link.is_symlink())
            self.assertTrue(runtime_internal_link.is_symlink())
            self.assertTrue(runtime_external_file.is_file())
            self.assertFalse(runtime_external_file.is_symlink())
            self.assertEqual(json.loads(evidence.read_text()), inventory)
            materialized = inventory["materialized"]
            self.assertIsInstance(materialized, list)
            self.assertEqual(
                materialized[0]["source_record"], regular_file_record(external)
            )
            self.assertEqual(
                materialized[0]["source_record"]["sha256"],
                materialized[0]["file"]["sha256"],
            )
            self.assertEqual(
                pipeline_module._verify_runtime_image(
                    inventory, buck_out, evidence.parent
                ),
                runtime_elf,
            )
            pipeline_module._verify_runtime_dependency_closure(
                runtime_elf, buck_out, closure
            )

            wrong_root = json.loads(json.dumps(inventory))
            wrong_root["runtime_root"] = str(runtime_elf.parent)
            with self.assertRaisesRegex(RuntimeError, "root differs"):
                pipeline_module._verify_runtime_image(
                    wrong_root, buck_out, evidence.parent
                )

            substitute_tree = root / "substitute-tree"
            substitute_tree.mkdir()
            wrong_tree = json.loads(json.dumps(inventory))
            wrong_tree["source_tree"] = str(substitute_tree)
            with self.assertRaisesRegex(RuntimeError, "paths are malformed"):
                pipeline_module._verify_runtime_image(
                    wrong_tree, buck_out, evidence.parent
                )

            runtime_elf.write_bytes(b"other-elf")
            wrong_elf = json.loads(json.dumps(inventory))
            wrong_elf["runtime_elf"] = regular_file_record(runtime_elf)
            with self.assertRaisesRegex(RuntimeError, "ELF differs"):
                pipeline_module._verify_runtime_image(
                    wrong_elf, buck_out, evidence.parent
                )
            runtime_elf.write_bytes(b"elf")

            wrong_external = root / "external" / "wrong-external.so"
            wrong_external.write_bytes(b"wrong")
            external_link.unlink()
            external_link.symlink_to(wrong_external)
            wrong_original = json.loads(json.dumps(inventory))
            wrong_original_entry = next(
                value
                for value in wrong_original["entries"]
                if value["name"] == external_link.name
            )
            wrong_original_entry["original_link"] = (
                pipeline_module._runtime_symlink_record(external_link)
            )
            with self.assertRaisesRegex(RuntimeError, "original link source differs"):
                pipeline_module._verify_runtime_image(
                    wrong_original, buck_out, evidence.parent
                )
            external_link.unlink()
            external_link.symlink_to(external)

            alternate_internal = internal.with_name("libalternate.so")
            alternate_internal.write_bytes(b"alternate")
            runtime_internal_link.unlink()
            runtime_internal_link.symlink_to(alternate_internal)
            wrong_internal = json.loads(json.dumps(inventory))
            wrong_internal_entry = next(
                value
                for value in wrong_internal["entries"]
                if value["name"] == internal_link.name
            )
            wrong_internal_entry["runtime"] = pipeline_module._runtime_symlink_record(
                runtime_internal_link
            )
            wrong_internal["closure"] = pipeline_module._runtime_dependency_closure(
                runtime_elf, buck_out
            )
            with self.assertRaisesRegex(RuntimeError, "internal runtime link source"):
                pipeline_module._verify_runtime_image(
                    wrong_internal, buck_out, evidence.parent
                )
            runtime_internal_link.unlink()
            runtime_internal_link.symlink_to(internal)

            internal.write_bytes(b"tampered")
            with self.assertRaisesRegex(RuntimeError, "closure differs"):
                pipeline_module._verify_runtime_dependency_closure(
                    runtime_elf, buck_out, closure
                )
            internal.write_bytes(b"internal")

            runtime_internal_link.unlink()
            with self.assertRaisesRegex(RuntimeError, "closure differs"):
                pipeline_module._verify_runtime_dependency_closure(
                    runtime_elf, buck_out, closure
                )
            runtime_internal_link.symlink_to(internal)

            runtime_internal_link.unlink()
            runtime_internal_link.symlink_to(external)
            with self.assertRaisesRegex(RuntimeError, "escaped buck-out"):
                pipeline_module._verify_runtime_dependency_closure(
                    runtime_elf, buck_out, closure
                )
            runtime_internal_link.unlink()
            runtime_internal_link.mkdir()
            with self.assertRaisesRegex(RuntimeError, "unsupported type"):
                pipeline_module._verify_runtime_dependency_closure(
                    runtime_elf, buck_out, closure
                )

    def test_runtime_image_stages_and_seals_executable_relative_resources(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            buck_out = checkout / "buck-out"
            output = buck_out / "v2" / "art" / "target" / "__test__"
            output.mkdir(parents=True)
            elf = output / "test"
            elf.write_bytes(b"elf")
            (output / "__test__shared_libs_symlink_tree").mkdir()
            certificate = checkout / "fbcode" / "certs" / "test.pem"
            certificate.parent.mkdir(parents=True)
            certificate.write_bytes(b"certificate\n")
            source_manifest = elf.with_name(elf.name + ".resources.json")
            logical = "fbcode/certs/test.pem"
            source_manifest.write_text(
                json.dumps(
                    {logical: os.path.relpath(certificate, source_manifest.parent)}
                )
            )
            evidence = root / "attempt" / "runtime.json"
            inventory = pipeline_module._materialize_external_runtime_links(
                elf, buck_out, evidence
            )
            resources = inventory["resources"]
            self.assertIsInstance(resources, dict)
            runtime_elf_record = inventory["runtime_elf"]
            self.assertIsInstance(runtime_elf_record, dict)
            runtime_elf = Path(str(runtime_elf_record["path"]))
            runtime_manifest = runtime_elf.with_name(
                runtime_elf.name + ".resources.json"
            )
            rewritten = json.loads(runtime_manifest.read_text())
            staged_resource = (runtime_manifest.parent / rewritten[logical]).resolve()
            self.assertTrue(
                staged_resource.is_relative_to(Path(str(inventory["runtime_root"])))
            )
            self.assertEqual(staged_resource.read_bytes(), certificate.read_bytes())
            self.assertEqual(
                pipeline_module._verify_runtime_image(
                    inventory, buck_out, evidence.parent
                ),
                runtime_elf,
            )
            staged_resource.write_bytes(b"tampered\n")
            with self.assertRaises((RuntimeError, StateError)):
                pipeline_module._verify_runtime_image(
                    inventory, buck_out, evidence.parent
                )

    def test_runtime_resource_manifest_rejects_malformed_and_unsafe_inputs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = root / "test.resources.json"
            rejected = {
                "duplicate": '{"a":"one","a":"two"}',
                "non-string": '{"a":1}',
                "unsafe-logical": '{"../a":"file"}',
                "absolute-target": '{"a":"/etc/passwd"}',
                "trailing-json": '{"a":"file"} true',
            }
            for name, content in rejected.items():
                with self.subTest(manifest=name):
                    manifest.write_text(content)
                    with self.assertRaises(RuntimeError):
                        pipeline_module._strict_resource_manifest(manifest)

    def test_runtime_image_rejects_parent_escape_and_recovery_delete_escape(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            buck_out = root / "checkout" / "buck-out"
            output = buck_out / "v2" / "art" / "target" / "__test__"
            output.mkdir(parents=True)
            elf = output / "test"
            elf.write_bytes(b"elf")
            tree = output / "__test__shared_libs_symlink_tree"
            tree.mkdir()
            source = root / "external" / "libexternal.so"
            source.parent.mkdir()
            source.write_bytes(b"external")
            (tree / source.name).symlink_to(source)
            runtime_parent = buck_out / ".tfa-runtime"
            outside = root / "outside"
            outside.mkdir()
            runtime_parent.symlink_to(outside)
            evidence = root / "symlink-attempt" / "runtime.json"
            with self.assertRaisesRegex(RuntimeError, "pinned directory"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, evidence
                )
            self.assertEqual(tuple(outside.iterdir()), ())
            runtime_parent.unlink()
            benign_parent = buck_out / ".tfa-runtime.real"
            benign_parent.mkdir()
            runtime_parent.symlink_to(benign_parent, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "pinned directory"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, evidence
                )
            self.assertEqual(tuple(benign_parent.iterdir()), ())
            runtime_parent.unlink()
            benign_parent.rmdir()

            aliased_checkout = root / "aliased-checkout"
            aliased_checkout.symlink_to(buck_out.parent, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "buck-out is not a pinned"):
                pipeline_module._materialize_external_runtime_links(
                    elf,
                    aliased_checkout / "buck-out",
                    root / "ancestor-symlink-attempt" / "runtime.json",
                )

            evidence = root / "replacement-attempt" / "runtime.json"
            inventory = pipeline_module._materialize_external_runtime_links(
                elf, buck_out, evidence
            )
            original_parent = buck_out / ".tfa-runtime.original"
            runtime_parent.rename(original_parent)
            runtime_parent.mkdir()
            with self.assertRaisesRegex(RuntimeError, "parent identity differs"):
                pipeline_module._verify_runtime_image(
                    inventory, buck_out, evidence.parent
                )
            runtime_parent.rmdir()
            original_parent.rename(runtime_parent)

            recovery_evidence = root / "recovery-attempt" / "runtime.json"
            real_copy = pipeline_module._copy_pinned_runtime_source

            def fail_external_copy(
                source_path: Path, destination: Path, max_bytes: int
            ) -> tuple[dict[str, object], dict[str, object]]:
                if destination.name == source.name:
                    raise RuntimeError("injected external-copy failure")
                return real_copy(source_path, destination, max_bytes)

            with (
                patch(
                    "testinfra.tfa.pipeline._copy_pinned_runtime_source",
                    side_effect=fail_external_copy,
                ),
                self.assertRaisesRegex(RuntimeError, "external-copy failure"),
            ):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, recovery_evidence
                )
            transaction = json.loads(
                recovery_evidence.with_name(
                    "runtime-materialization-transaction.json"
                ).read_text()
            )
            staging_name = Path(str(transaction["staging"])).name
            staging = runtime_parent / staging_name
            held_staging = runtime_parent / f"{staging_name}.held"
            staging.rename(held_staging)
            staging.mkdir()
            replacement_sentinel = staging / "replacement-sentinel"
            replacement_sentinel.write_text("preserve")
            with self.assertRaisesRegex(RuntimeError, "directory differs"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, recovery_evidence
                )
            self.assertEqual(replacement_sentinel.read_text(), "preserve")
            replacement_sentinel.unlink()
            staging.rmdir()
            held_staging.rename(staging)
            child = next(path for path in staging.iterdir() if path.is_dir())
            held_child = staging / f"{child.name}.held"
            child.rename(held_child)
            replacement_child = root / "outside-replacement-child"
            replacement_child.mkdir()
            child_sentinel = replacement_child / "outside-sentinel"
            child_sentinel.write_text("preserve")
            replacement_child.rename(child)
            before_recovery = tuple(
                sorted(path.name for path in runtime_parent.iterdir())
            )
            with self.assertRaisesRegex(RuntimeError, "requires explicit cleanup"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, recovery_evidence
                )
            self.assertEqual((child / child_sentinel.name).read_text(), "preserve")
            recovered_transaction = json.loads(
                recovery_evidence.with_name(
                    "runtime-materialization-transaction.json"
                ).read_text()
            )
            preserved = recovered_transaction["preserved_staging"]
            self.assertIsInstance(preserved, list)
            self.assertEqual([record["path"] for record in preserved], [str(staging)])
            self.assertEqual(recovered_transaction["phase"], "preserved")
            with self.assertRaisesRegex(RuntimeError, "requires explicit cleanup"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, recovery_evidence
                )
            self.assertEqual(
                tuple(sorted(path.name for path in runtime_parent.iterdir())),
                before_recovery,
            )
            self.assertEqual((child / child_sentinel.name).read_text(), "preserve")
            escaped_parent = root / "escaped-runtime-parent"
            escaped_parent.mkdir()
            escaped_staging = escaped_parent / staging_name
            escaped_staging.mkdir()
            sentinel = escaped_staging / "sentinel"
            sentinel.write_text("preserve")
            held_parent = buck_out / ".tfa-runtime.held"
            runtime_parent.rename(held_parent)
            runtime_parent.symlink_to(escaped_parent)
            with self.assertRaisesRegex(RuntimeError, "pinned directory"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, recovery_evidence
                )
            self.assertEqual(sentinel.read_text(), "preserve")

    def test_runtime_image_recovers_all_multi_entry_publication_boundaries(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            buck_out = root / "checkout" / "buck-out"
            output = buck_out / "v2" / "art" / "target" / "__test__"
            output.mkdir(parents=True)
            elf = output / "test"
            elf.write_bytes(b"elf")
            tree = output / "__test__shared_libs_symlink_tree"
            tree.mkdir()
            sources: list[Path] = []
            links: list[Path] = []
            for name in ("libexternal-one.so", "libexternal-two.so"):
                source = root / "external" / name
                source.parent.mkdir(exist_ok=True)
                source.write_bytes(name.encode())
                link = tree / name
                link.symlink_to(source)
                sources.append(source)
                links.append(link)

            def assert_complete(evidence: Path) -> dict[str, object]:
                inventory = pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, evidence
                )
                transaction = json.loads(
                    evidence.with_name(
                        "runtime-materialization-transaction.json"
                    ).read_text()
                )
                self.assertEqual(transaction["phase"], "published")
                self.assertFalse(Path(str(transaction["staging"])).exists())
                materialized = inventory["materialized"]
                self.assertIsInstance(materialized, list)
                self.assertEqual(len(materialized), 2)
                self.assertEqual(
                    sorted(str(value["source"]) for value in materialized),
                    sorted(str(source) for source in sources),
                )
                entries = inventory["entries"]
                self.assertIsInstance(entries, list)
                self.assertEqual(sum(value["external"] is True for value in entries), 2)
                self.assertTrue(all(link.is_symlink() for link in links))
                runtime_tree = Path(str(inventory["tree"]))
                self.assertTrue(
                    all((runtime_tree / link.name).is_file() for link in links)
                )
                return inventory

            copy_evidence = root / "copy-failure" / "runtime.json"
            real_copy = pipeline_module._copy_pinned_runtime_source

            def fail_second_copy(
                source: Path, destination: Path, max_bytes: int
            ) -> tuple[dict[str, object], dict[str, object]]:
                if destination.name == links[1].name:
                    raise RuntimeError("injected second-copy failure")
                return real_copy(source, destination, max_bytes)

            with (
                patch(
                    "testinfra.tfa.pipeline._copy_pinned_runtime_source",
                    side_effect=fail_second_copy,
                ),
                self.assertRaisesRegex(RuntimeError, "second-copy failure"),
            ):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, copy_evidence
                )
            self.assertTrue(all(link.is_symlink() for link in links))
            with self.assertRaisesRegex(RuntimeError, "requires explicit cleanup"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, copy_evidence
                )

            inventory_evidence = root / "inventory-failure" / "runtime.json"
            real_write = pipeline_module.write_metadata
            inventory_failed = False

            def fail_after_inventory(target: Path, value: dict[str, object]) -> None:
                nonlocal inventory_failed
                real_write(target, value)
                if target == inventory_evidence and not inventory_failed:
                    inventory_failed = True
                    raise RuntimeError("injected inventory failure")

            with (
                patch(
                    "testinfra.tfa.pipeline.write_metadata",
                    side_effect=fail_after_inventory,
                ),
                self.assertRaisesRegex(RuntimeError, "inventory failure"),
            ):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, inventory_evidence
                )
            with self.assertRaisesRegex(RuntimeError, "requires explicit cleanup"):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, inventory_evidence
                )

            publish_evidence = root / "publish-failure" / "runtime.json"
            with (
                patch(
                    "testinfra.tfa.pipeline._publish_runtime_image",
                    side_effect=RuntimeError("injected publish failure"),
                ),
                self.assertRaisesRegex(RuntimeError, "publish failure"),
            ):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, publish_evidence
                )
            assert_complete(publish_evidence)

            verify_evidence = root / "post-publish-failure" / "runtime.json"
            with (
                patch(
                    "testinfra.tfa.pipeline._verify_runtime_image",
                    side_effect=RuntimeError("injected post-publish failure"),
                ),
                self.assertRaisesRegex(RuntimeError, "post-publish failure"),
            ):
                pipeline_module._materialize_external_runtime_links(
                    elf, buck_out, verify_evidence
                )
            assert_complete(verify_evidence)

            sync_evidence = root / "durability" / "runtime.json"
            real_sync = os.fsync
            synced_directories: list[tuple[int, int]] = []

            def record_sync(descriptor: int) -> None:
                metadata = os.fstat(descriptor)
                if stat.S_ISDIR(metadata.st_mode):
                    synced_directories.append((metadata.st_dev, metadata.st_ino))
                real_sync(descriptor)

            with patch("testinfra.tfa.pipeline.os.fsync", side_effect=record_sync):
                inventory = assert_complete(sync_evidence)
            runtime_parent = Path(str(inventory["runtime_root"])).parent
            self.assertIn(
                (runtime_parent.stat().st_dev, runtime_parent.stat().st_ino),
                synced_directories,
            )
            self.assertIn(
                (buck_out.stat().st_dev, buck_out.stat().st_ino),
                synced_directories,
            )

    def test_httpcoro_fatal_witness_uses_exact_abnormal_termination(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repro_root = Path(temporary)
            case = load_manifest(
                Path(manifest_module.__file__).with_name("cases.v1.json")
            ).case("httpcoro")
            for outcome, manifest_argv, returncode in (
                ("pass", case.pass_argv, 0),
                ("fail", case.fail_argv, 134),
            ):
                root = repro_root / "httpcoro" / outcome
                controller = root / "controller-cwd"
                controller.mkdir(parents=True)
                seal_working_directory(controller, root / "controller-cwd.before.json")
                seal_directory_evidence(controller, root / "controller-cwd.after.json")
                (root / "wrapper").mkdir()
                argv = hermit_attempt_argv(
                    hermit_witness_argv(manifest_argv, root),
                    Path(case.working_directory),
                    controller,
                    180,
                    safehermit_wrapper_path(root),
                )
                identity = pipeline_module._gtest_identity(argv)
                stdout = root / "wrapper" / "stdout.txt"
                stderr = root / "wrapper" / "stderr.txt"
                if outcome == "pass":
                    stdout.write_text(
                        f"Note: Google Test filter = {identity}\n"
                        "[==========] Running 1 test from 1 test suite.\n"
                        f"[ RUN      ] {identity}\n"
                        f"[       OK ] {identity} (1 ms)\n"
                        "[  PASSED  ] 1 test.\n"
                    )
                    stderr.write_text("")
                else:
                    stdout.write_text(
                        f"Note: Google Test filter = {identity}\n"
                        "[==========] Running 1 test from 1 test suite.\n"
                        "[----------] Global test environment set-up.\n"
                        "[----------] 1 test from "
                        "HTTPDownstreamSessionTest/H2DownstreamSessionTest\n"
                        f"[ RUN      ] {identity}\n"
                    )
                    stderr.write_text(
                        ":: Comparing captured verification logs...\n"
                        ":: comparison=BitwiseInfoV1 relaxations=none\n"
                        ":: Success: deterministic. Determinism verified.\n"
                        "F0921 21:01:54.726478     3 "
                        "HTTPDownstreamCoroSessionTests.cpp:1685] "
                        "Check failed: bodyEvent.hasException() \n"
                        "*** Check failure stack trace: ***\n"
                        "*** Signal 6 (SIGABRT) received by PID 3 ***\n"
                        "    @ 0000000000eb36c2 proxygen::coro::test::"
                        "H2DownstreamSessionTest_ResetRateLimit_Test::co_TestBody()"
                        "::$_1::operator()(proxygen::coro::HTTPSourceHolder) const "
                        "[clone .resume]\n"
                        "                       ./fbcode/proxygen/lib/http/coro/"
                        "test/HTTPDownstreamCoroSessionTests.cpp:1685\n"
                        "*** Check failure async stack trace: ***\n"
                        "    @ 0000000000eb36c3 proxygen::coro::test::"
                        "H2DownstreamSessionTest_ResetRateLimit_Test::co_TestBody()"
                        "::$_1::operator()(proxygen::coro::HTTPSourceHolder) const "
                        "[clone .resume]\n"
                        "                       ./fbcode/proxygen/lib/http/coro/"
                        "test/HTTPDownstreamCoroSessionTests.cpp:1685\n"
                    )
                metadata, argv = _write_safehermit_fixture(
                    root, argv, returncode, case.hermit_sha256
                )
                metadata.update(
                    {
                        "containment_empty": True,
                        "cwd": str(controller),
                        "payload_isolation": "safehermit",
                    }
                )
                write_metadata(root / "wrapper" / "metadata.json", metadata)
                write_metadata(
                    root / "working-directory.json",
                    {
                        "controller_evidence": controller_evidence(root),
                        "outer_timeout_seconds": 180,
                        "working_directory": case.working_directory,
                    },
                )
            with patch("testinfra.tfa.pipeline._assert_original_artifact"):
                evidence = validate_witness_artifacts(case, repro_root)
            fatal = evidence["fail"]
            self.assertIsInstance(fatal, dict)
            self.assertEqual(fatal["returncode"], 134)
            self.assertEqual(fatal["gtest_execution"]["mode"], "fatal-sigabrt")
            self.assertEqual(fatal["safehermit"]["exit_code"], 134)

            fail_root = repro_root / "httpcoro" / "fail"
            fail_stdout = fail_root / "wrapper" / "stdout.txt"
            original_stdout = fail_stdout.read_text()
            rejected_stdout = (
                original_stdout
                + f"[  FAILED  ] {fatal['gtest_execution']['filter']} (1 ms)\n"
            )
            fail_stdout.write_text(rejected_stdout)
            with self.assertRaisesRegex(RuntimeError, "fatal gtest did not start"):
                _gtest_fatal_execution_evidence(
                    tuple(
                        json.loads((fail_root / "wrapper/metadata.json").read_text())[
                            "argv"
                        ]
                    ),
                    fail_stdout,
                    fail_root / "wrapper" / "stderr.txt",
                    pipeline_module.FAILURE_MARKERS["httpcoro"],
                )
            fail_stdout.write_text(original_stdout)
            fail_stderr = fail_root / "wrapper" / "stderr.txt"
            original_stderr = fail_stderr.read_text()
            first_marker, second_marker = pipeline_module.FAILURE_MARKERS["httpcoro"]
            rejected_stderr = {
                "duplicate": original_stderr.replace(
                    "Check failed: bodyEvent.hasException()",
                    "Check failed: bodyEvent.hasException() "
                    "Check failed: bodyEvent.hasException()",
                ),
                "reordered": (
                    f"Check failed: {second_marker}()\n{first_marker}\n"
                    "*** Signal 6 (SIGABRT) received by PID 3 ***\n"
                ),
                "signal-first": (
                    "*** Signal 6 (SIGABRT) received by PID 3 ***\n"
                    f"{first_marker} {second_marker}\n"
                ),
            }
            for mutation, transcript in rejected_stderr.items():
                with self.subTest(fatal_transcript=mutation):
                    fail_stderr.write_text(transcript)
                    with self.assertRaisesRegex(
                        RuntimeError, "fatal gtest did not start"
                    ):
                        _gtest_fatal_execution_evidence(
                            tuple(
                                json.loads(
                                    (fail_root / "wrapper/metadata.json").read_text()
                                )["argv"]
                            ),
                            fail_stdout,
                            fail_stderr,
                            pipeline_module.FAILURE_MARKERS["httpcoro"],
                        )
            fail_stderr.write_text(original_stderr)
            fail_stdout.write_text(original_stdout + first_marker + "\n")
            with self.assertRaisesRegex(RuntimeError, "fatal gtest did not start"):
                _gtest_fatal_execution_evidence(
                    tuple(
                        json.loads((fail_root / "wrapper/metadata.json").read_text())[
                            "argv"
                        ]
                    ),
                    fail_stdout,
                    fail_stderr,
                    pipeline_module.FAILURE_MARKERS["httpcoro"],
                )
            fail_stdout.write_text(original_stdout)

            fatal_mutations = {
                "missing-normal-frame": original_stderr.replace(
                    "H2DownstreamSessionTest_ResetRateLimit_Test::co_TestBody()"
                    "::$_1::operator()(proxygen::coro::HTTPSourceHolder) const "
                    "[clone .resume]",
                    "DifferentTest::co_TestBody()",
                    1,
                ),
                "extra-location": original_stderr
                + "HTTPDownstreamCoroSessionTests.cpp:1685\n",
                "missing-async-delimiter": original_stderr.replace(
                    "*** Check failure async stack trace: ***",
                    "*** unrelated async stack trace: ***",
                ),
                "weakened-compare": original_stderr.replace(
                    ":: comparison=BitwiseInfoV1 relaxations=none",
                    ":: comparison=BitwiseInfoV1 relaxations=addresses",
                ),
            }
            for mutation, transcript in fatal_mutations.items():
                with self.subTest(role_mutation=mutation):
                    fail_stderr.write_text(transcript)
                    with self.assertRaisesRegex(
                        RuntimeError, "fatal gtest did not start"
                    ):
                        _gtest_fatal_execution_evidence(
                            tuple(
                                json.loads(
                                    (fail_root / "wrapper/metadata.json").read_text()
                                )["argv"]
                            ),
                            fail_stdout,
                            fail_stderr,
                            pipeline_module.FAILURE_MARKERS["httpcoro"],
                        )
            fail_stderr.write_text(original_stderr)

            report = fail_root / "safehermit.json"
            original_report = report.read_text()
            report.write_text(
                original_report.replace("truncated=false", "truncated=true")
            )
            metadata = json.loads((fail_root / "wrapper/metadata.json").read_text())
            metadata["finished_wall_time_ns"] = time.time_ns()
            with self.assertRaisesRegex(HermitError, "safety or completion"):
                safehermit_evidence(
                    tuple(metadata["argv"]), metadata, report, case.hermit_sha256
                )
            report.write_text(
                "\n".join(
                    line
                    for line in original_report.splitlines()
                    if not line.startswith("safehermit: exit_code=")
                )
                + "\n"
            )
            metadata["finished_wall_time_ns"] = time.time_ns()
            with self.assertRaisesRegex(HermitError, "incomplete"):
                safehermit_evidence(
                    tuple(metadata["argv"]), metadata, report, case.hermit_sha256
                )
            original_run_id = fatal["safehermit"]["run_id"]
            self.assertIsInstance(original_run_id, str)
            run_stamp = original_run_id.rsplit("-", maxsplit=1)[0]
            report.write_text(
                original_report.replace(original_run_id, f"{run_stamp}-99999")
            )
            metadata["finished_wall_time_ns"] = time.time_ns()
            with self.assertRaisesRegex(HermitError, "differs from its payload"):
                safehermit_evidence(
                    tuple(metadata["argv"]), metadata, report, case.hermit_sha256
                )
            report.write_text(
                original_report.replace(original_run_id, "20000101T000000Z-12345")
            )
            metadata["finished_wall_time_ns"] = time.time_ns()
            with self.assertRaisesRegex(HermitError, "not bound to this process"):
                safehermit_evidence(
                    tuple(metadata["argv"]), metadata, report, case.hermit_sha256
                )
            report.write_text(original_report)
            stale = int(metadata["started_wall_time_ns"]) - 2_000_000_000
            os.utime(report, ns=(stale, stale))
            with self.assertRaisesRegex(HermitError, "not fresh"):
                safehermit_evidence(
                    tuple(metadata["argv"]), metadata, report, case.hermit_sha256
                )
            os.utime(report, ns=(int(metadata["started_wall_time_ns"]),) * 2)
            for rejected_exit in (124, 125, 126):
                with self.subTest(rejected_exit=rejected_exit):
                    report.write_text(
                        original_report.replace(
                            "safehermit: exit_code=134",
                            f"safehermit: exit_code={rejected_exit}",
                        )
                    )
                    metadata["returncode"] = rejected_exit
                    metadata["finished_wall_time_ns"] = time.time_ns()
                    with self.assertRaises(HermitError):
                        safehermit_evidence(
                            tuple(metadata["argv"]),
                            metadata,
                            report,
                            case.hermit_sha256,
                        )
            metadata["returncode"] = 134
            report.write_text(original_report)
            metadata["finished_wall_time_ns"] = time.time_ns()
            for runtime_input in safehermit_runtime_paths(fail_root):
                with self.subTest(runtime_input=runtime_input.name):
                    original = runtime_input.read_bytes()
                    runtime_input.unlink()
                    runtime_input.write_bytes(original + b"\n# replaced\n")
                    runtime_input.chmod(0o500)
                    with self.assertRaisesRegex(HermitError, "execution input differs"):
                        safehermit_evidence(
                            tuple(metadata["argv"]),
                            metadata,
                            report,
                            case.hermit_sha256,
                        )
                    runtime_input.unlink()
                    runtime_input.write_bytes(original)
                    runtime_input.chmod(0o500)

    def test_gtest_execution_evidence_rejects_nonexecution_modes(self) -> None:
        argv = ("safehermit", "--", "/tmp/test", "--gtest_filter=Suite.Test")
        with tempfile.TemporaryDirectory() as temporary:
            stdout = Path(temporary) / "stdout.txt"
            stdout.write_text(
                "Note: Google Test filter = Suite.Test\n"
                "[==========] Running 1 test from 1 test suite.\n"
                "[ RUN      ] Suite.Test\n"
                "[       OK ] Suite.Test (1 ms)\n"
                "[  PASSED  ] 1 test.\n"
            )
            evidence = pipeline_module._gtest_execution_evidence(argv, stdout, True)
            self.assertEqual(evidence["filter"], "Suite.Test")
            rejected = {
                "deleted": (
                    "Note: Google Test filter = Suite.Test\n"
                    "[==========] Running 0 tests from 0 test suites.\n"
                    "[  PASSED  ] 0 tests.\n"
                ),
                "renamed": (
                    "Note: Google Test filter = Suite.Test\n"
                    "[==========] Running 1 test from 1 test suite.\n"
                    "[ RUN      ] Suite.Renamed\n"
                    "[       OK ] Suite.Renamed (1 ms)\n"
                    "[  PASSED  ] 1 test.\n"
                ),
                "disabled": (
                    "Note: Google Test filter = Suite.Test\n"
                    "[==========] Running 0 tests from 0 test suites.\n"
                    "  YOU HAVE 1 DISABLED TEST\n"
                    "[  PASSED  ] 0 tests.\n"
                ),
                "filter-miss": (
                    "Note: Google Test filter = Other.Test\n"
                    "[==========] Running 1 test from 1 test suite.\n"
                    "[ RUN      ] Other.Test\n"
                    "[       OK ] Other.Test (1 ms)\n"
                    "[  PASSED  ] 1 test.\n"
                ),
                "mixed-fake-and-zero": (
                    "Note: Google Test filter = Suite.Test\n"
                    "[==========] Running 1 test from 1 test suite.\n"
                    "[ RUN      ] Suite.Test\n"
                    "[       OK ] Suite.Test (1 ms)\n"
                    "[  PASSED  ] 1 test.\n"
                    "[==========] Running 0 tests from 0 test suites.\n"
                    "[  PASSED  ] 0 tests.\n"
                ),
                "additional-wrong-test": (
                    "Note: Google Test filter = Suite.Test\n"
                    "[==========] Running 1 test from 1 test suite.\n"
                    "[ RUN      ] Suite.Test\n"
                    "[ RUN      ] Other.Test\n"
                    "[       OK ] Suite.Test (1 ms)\n"
                    "[       OK ] Other.Test (1 ms)\n"
                    "[  PASSED  ] 1 test.\n"
                ),
                "duplicate-expected-run": (
                    "Note: Google Test filter = Suite.Test\n"
                    "[==========] Running 1 test from 1 test suite.\n"
                    "[ RUN      ] Suite.Test\n"
                    "[ RUN      ] Suite.Test\n"
                    "[       OK ] Suite.Test (1 ms)\n"
                    "[  PASSED  ] 1 test.\n"
                ),
            }
            for name, output in rejected.items():
                with self.subTest(name=name):
                    stdout.write_text(output)
                    with self.assertRaisesRegex(
                        RuntimeError, "selected gtest did not execute exactly once"
                    ):
                        pipeline_module._gtest_execution_evidence(argv, stdout, True)
            failure_output = (
                "Note: Google Test filter = Suite.Test\n"
                "[==========] Running 1 test from 1 test suite.\n"
                "[ RUN      ] Suite.Test\n"
                "[  FAILED  ] Suite.Test (1 ms)\n"
                "[  PASSED  ] 0 tests.\n"
                "[  FAILED  ] 1 test, listed below:\n"
                "[  FAILED  ] Suite.Test\n"
            )
            stdout.write_text(failure_output)
            failure_evidence = pipeline_module._gtest_execution_evidence(
                argv, stdout, False
            )
            self.assertEqual(
                failure_evidence["terminal_line"],
                "[  FAILED  ] Suite.Test (1 ms)",
            )
            rejected_failures = {
                "mixed-fake-and-zero": (
                    failure_output
                    + "[==========] Running 0 tests from 0 test suites.\n"
                    "[  PASSED  ] 0 tests.\n"
                ),
                "additional-wrong-test": failure_output.replace(
                    "[  FAILED  ] Suite.Test (1 ms)\n",
                    "[ RUN      ] Other.Test\n"
                    "[  FAILED  ] Suite.Test (1 ms)\n"
                    "[  FAILED  ] Other.Test (1 ms)\n",
                ),
                "duplicate-expected-run": failure_output.replace(
                    "[ RUN      ] Suite.Test\n",
                    "[ RUN      ] Suite.Test\n[ RUN      ] Suite.Test\n",
                ),
            }
            for name, output in rejected_failures.items():
                with self.subTest(name=f"failure-{name}"):
                    stdout.write_text(output)
                    with self.assertRaisesRegex(
                        RuntimeError, "selected gtest did not execute exactly once"
                    ):
                        pipeline_module._gtest_execution_evidence(argv, stdout, False)

    def test_candidate_handoff_rejects_artifact_checkout_overlap(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary) / "checkout"
            checkout.mkdir()
            with (
                patch("testinfra.tfa.pipeline.run_process") as process,
                self.assertRaisesRegex(RuntimeError, "checkout overlap"),
            ):
                diagnose_candidate(
                    _loaded_case(),
                    checkout / "artifacts" / "case",
                    checkout,
                    Path(temporary) / "schema.json",
                    120,
                )
            process.assert_not_called()

    @_serialized_unit_working_directory
    def test_codex_candidate_handoff_precedes_controller_apply(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            case_dir = root / "artifacts" / case.case_id
            checkout = root / "checkout"
            checkout.mkdir()
            schema = root / "report.schema.json"
            schema.write_text("{}\n")
            events: list[str] = []

            def execute(
                executable: str,
                arguments: tuple[str, ...],
                cwd: Path,
                artifact_dir: Path,
                _timeout: float,
                *_rest: object,
            ) -> ProcessResult:
                phase = "agent-incomplete" if not events else "agent-remediated"
                events.append(phase)
                self.assertEqual(cwd, case_dir / "diagnostic-workspace")
                self.assertIn("sandbox_workspace_write.network_access=false", arguments)
                self.assertNotEqual(cwd, checkout)
                output = case_dir / "agent-output"
                output.mkdir(parents=True, exist_ok=True)
                if phase == "agent-remediated":
                    self.assertIn("resume", arguments)
                    self.assertEqual(_timeout, 90)
                    ReportTest._write_artifacts(output)
                else:
                    self.assertEqual(_timeout, 120)
                final_message = Path(
                    arguments[arguments.index("--output-last-message") + 1]
                )
                final_message.parent.mkdir(parents=True, exist_ok=True)
                final_message.write_text(
                    json.dumps(_valid_report(case.case_id, output))
                )
                artifact_dir.mkdir(parents=True, exist_ok=True)
                stdout = artifact_dir / "stdout.txt"
                stderr = artifact_dir / "stderr.txt"
                with stdout.open("a") as stream:
                    stream.write('{"thread_id":"session"}\n')
                stderr.write_text("")
                write_metadata(
                    artifact_dir / "metadata.json",
                    {
                        "argv": [executable, *arguments],
                        "phase": phase,
                        "state": "completed",
                        "timeout_seconds": _timeout,
                    },
                )
                return ProcessResult(
                    (executable, *arguments), 0, 1.0, False, stdout, stderr
                )

            def apply(*_args: object) -> None:
                self.assertEqual(
                    events,
                    ["agent-incomplete", "agent-remediated", "review-approved"],
                )
                events.append("controller-applied")

            def approve(*_args: object) -> None:
                self.assertEqual(events, ["agent-incomplete", "agent-remediated"])
                events.append("review-approved")

            qualified = ReportSummary(
                case.case_id,
                "controller-qualified",
                len(case.fresh_stress_seeds),
                len(case.fresh_stress_seeds),
                str(case_dir / "agent-output" / "patch.diff"),
            )

            with (
                patch("testinfra.tfa.pipeline._active_candidate", return_value=None),
                patch(
                    "testinfra.tfa.pipeline._checkout_state",
                    side_effect=((), (), ()),
                ),
                patch(
                    "testinfra.tfa.pipeline.time.monotonic",
                    side_effect=(100.0, 130.0),
                ),
                patch(
                    "testinfra.tfa.pipeline.run_process", side_effect=execute
                ) as diagnostic_process,
                patch(
                    "testinfra.tfa.pipeline._approve_candidate_locked",
                    side_effect=approve,
                ),
                patch(
                    "testinfra.tfa.pipeline._apply_candidate_locked",
                    side_effect=apply,
                ),
            ):
                candidate = diagnose_candidate(case, case_dir, checkout, schema, 120)
                self.assertEqual(events, ["agent-incomplete", "agent-remediated"])
                remediation_path = (
                    case_dir
                    / "control"
                    / "candidate-attempts"
                    / "attempt-001"
                    / "remediation.json"
                )
                original_remediation = json.loads(remediation_path.read_text())
                mutations: dict[str, object] = {
                    "original_prompt_sha256": "0" * 64,
                    "remediation_prompt_sha256": "0" * 64,
                    "missing": [],
                    "session_id": "different-session",
                    "consumed_seconds": 31.0,
                    "original_timeout_seconds": 121.0,
                    "initial_timeout_seconds": 119.0,
                    "remaining_before_remediation_seconds": 89.0,
                    "deadline_epoch_seconds": (
                        float(original_remediation["deadline_epoch_seconds"]) + 1
                    ),
                    "remediation_timeout_seconds": 89.0,
                }
                for field, replacement in mutations.items():
                    with self.subTest(remediation_field=field):
                        changed_remediation = dict(original_remediation)
                        changed_remediation[field] = replacement
                        write_metadata(remediation_path, changed_remediation)
                        with self.assertRaises(RuntimeError):
                            pipeline_module._validate_remediation_transaction(
                                remediation_path.parent
                            )
                write_metadata(remediation_path, original_remediation)
                control = case_dir / "control"
                write_metadata(control / "witness-seal.json", {"sealed": True})
                ledger = Ledger(control / "ledger.jsonl")
                for stage in (
                    Stage.VALIDATED_REPRO,
                    Stage.INITIAL_COMMENT,
                    Stage.INVESTIGATING,
                ):
                    ledger.append(stage, {"test": True})
                with (
                    patch(
                        "testinfra.tfa.pipeline.validate_witness_artifacts",
                        return_value={"sealed": True},
                    ),
                    patch(
                        "testinfra.tfa.pipeline.independent_validate",
                        return_value=ValidationResult(
                            ("fbcode/product/fix.cpp",),
                            len(case.fresh_stress_seeds),
                            len(case.fresh_stress_seeds),
                        ),
                    ),
                    patch(
                        "testinfra.tfa.pipeline._validation_result",
                        return_value=ValidationResult(
                            ("fbcode/product/fix.cpp",),
                            len(case.fresh_stress_seeds),
                            len(case.fresh_stress_seeds),
                        ),
                    ),
                    patch(
                        "testinfra.tfa.pipeline._qualified_report",
                        return_value=qualified,
                    ),
                    patch(
                        "testinfra.tfa.pipeline._draft",
                        side_effect=RuntimeError("stop after verified"),
                    ),
                    self.assertRaisesRegex(RuntimeError, "stop after verified"),
                ):
                    run_case(
                        Manifest(1, (case,)),
                        case.case_id,
                        root / "artifacts",
                        root / "repro",
                        checkout,
                        schema,
                        None,
                        120,
                    )
            self.assertEqual(
                events,
                [
                    "agent-incomplete",
                    "agent-remediated",
                    "review-approved",
                    "controller-applied",
                ],
            )
            self.assertEqual(candidate.case_id, case.case_id)
            self.assertEqual(ledger.read()[-1].stage, Stage.VERIFIED)
            self.assertEqual(diagnostic_process.call_count, 2)
            remediation = json.loads(
                (
                    case_dir
                    / "control"
                    / "candidate-attempts"
                    / "attempt-001"
                    / "remediation.json"
                ).read_text()
            )
            self.assertEqual(remediation["phase"], "complete")
            self.assertNotEqual(
                remediation["initial_process"]["path"],
                remediation["remediation_process"]["path"],
            )
            self.assertEqual(
                json.loads(Path(remediation["initial_process"]["path"]).read_text())[
                    "phase"
                ],
                "agent-incomplete",
            )
            self.assertEqual(
                json.loads(
                    Path(remediation["remediation_process"]["path"]).read_text()
                )["phase"],
                "agent-remediated",
            )
            self.assertEqual(
                remediation["initial_process_argv"],
                json.loads(Path(remediation["initial_process"]["path"]).read_text())[
                    "argv"
                ],
            )
            self.assertEqual(
                remediation["remediation_process_argv"],
                json.loads(
                    Path(remediation["remediation_process"]["path"]).read_text()
                )["argv"],
            )
            self.assertTrue(
                (
                    case_dir
                    / "control"
                    / "candidate-attempts"
                    / "attempt-001"
                    / "candidate-seal.json"
                ).is_file()
            )
            incomplete = (
                case_dir
                / "control"
                / "incomplete-candidate-attempts"
                / "attempt-001"
                / "incomplete.json"
            )
            self.assertTrue(incomplete.is_file())
            self.assertIn("patch.diff", incomplete.read_text())

    @_serialized_unit_working_directory
    def test_codex_candidate_remediation_preserves_second_omission(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            case_dir = root / "artifacts" / case.case_id
            checkout = root / "checkout"
            checkout.mkdir()
            schema = root / "report.schema.json"
            schema.write_text("{}\n")
            calls = 0

            def execute(
                executable: str,
                arguments: tuple[str, ...],
                _cwd: Path,
                artifact_dir: Path,
                _timeout: float,
                *_rest: object,
            ) -> ProcessResult:
                nonlocal calls
                calls += 1
                output = case_dir / "agent-output"
                output.mkdir(parents=True, exist_ok=True)
                final_message = Path(
                    arguments[arguments.index("--output-last-message") + 1]
                )
                final_message.parent.mkdir(parents=True, exist_ok=True)
                final_message.write_text(
                    json.dumps(_valid_report(case.case_id, output))
                )
                artifact_dir.mkdir(parents=True, exist_ok=True)
                stdout = artifact_dir / "stdout.txt"
                stderr = artifact_dir / "stderr.txt"
                with stdout.open("a") as stream:
                    stream.write('{"thread_id":"session"}\n')
                stderr.write_text("")
                write_metadata(
                    artifact_dir / "metadata.json",
                    {
                        "argv": [executable, *arguments],
                        "call": calls,
                        "state": "completed",
                        "timeout_seconds": _timeout,
                    },
                )
                return ProcessResult(
                    (executable, *arguments), 0, 0.1, False, stdout, stderr
                )

            with (
                patch("testinfra.tfa.pipeline._active_candidate", return_value=None),
                patch(
                    "testinfra.tfa.pipeline._checkout_state", side_effect=((), (), ())
                ),
                patch(
                    "testinfra.tfa.pipeline.run_process", side_effect=execute
                ) as process,
            ):
                with self.assertRaisesRegex(
                    RuntimeError, "remediation left an invalid candidate"
                ):
                    _diagnose_candidate_locked(
                        case, case_dir, checkout, schema, 120, False
                    )
                for _invocation in range(2):
                    with self.assertRaisesRegex(RuntimeError, "explicit reset"):
                        diagnose_candidate(case, case_dir, checkout, schema, 120)
            self.assertEqual(calls, 2)
            self.assertEqual(process.call_count, 2)
            remediation = json.loads(
                (
                    case_dir
                    / "control"
                    / "candidate-attempts"
                    / "attempt-001"
                    / "remediation.json"
                ).read_text()
            )
            self.assertEqual(remediation["phase"], "terminal-failed")
            self.assertEqual(remediation["remaining_seconds"], 0)
            control = case_dir / "control" / "incomplete-candidate-attempts"
            archives = sorted(control.iterdir())
            self.assertEqual(len(archives), 3)
            issues = [
                json.loads((archive / "incomplete.json").read_text())["issue"]
                for archive in archives
            ]
            self.assertIn("patch.diff", issues[0])
            self.assertEqual(issues[1], "diagnostic remediation process completed")
            self.assertIn("patch.diff", issues[2])
            for archive in archives:
                self.assertTrue((archive / "process-stdout.txt").is_file())
                self.assertTrue((archive / "process-metadata.json").is_file())
                self.assertTrue((archive / "final-message.json").is_file())
                self.assertTrue((archive / "agent-report.json").is_file())
            self.assertFalse(
                (
                    case_dir
                    / "control"
                    / "candidate-attempts"
                    / "attempt-001"
                    / "candidate-seal.json"
                ).exists()
            )

    @_serialized_unit_working_directory
    def test_detected_remediation_crash_never_renews_public_deadline(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            case_dir = root / "artifacts" / case.case_id
            attempt = case_dir / "control" / "candidate-attempts" / "attempt-001"
            attempt.mkdir(parents=True)
            prompt = attempt / "prompt.txt"
            prompt.write_text("original prompt\n")
            process_metadata = (
                case_dir / "control" / "diagnostic-agent" / "metadata.json"
            )
            initial_argv = [
                "/usr/local/bin/codex",
                "exec",
                "--",
                "original prompt\n",
            ]
            write_metadata(
                process_metadata,
                {
                    "argv": initial_argv,
                    "state": "completed",
                    "timeout_seconds": 120.0,
                },
            )
            detected_epoch = time.time()
            pipeline_module._write_remediation_transaction(
                attempt,
                {
                    "attempt": str(attempt),
                    "consumed_seconds": 30.0,
                    "deadline_epoch_seconds": detected_epoch + 90,
                    "detected_epoch_seconds": detected_epoch,
                    "initial_process": regular_file_record(process_metadata),
                    "initial_process_argv": initial_argv,
                    "initial_timeout_seconds": 120.0,
                    "missing": ["patch.diff"],
                    "original_timeout_seconds": 120.0,
                    "original_prompt": regular_file_record(prompt),
                    "original_prompt_sha256": hashlib.sha256(
                        prompt.read_bytes()
                    ).hexdigest(),
                    "remaining_seconds": 90.0,
                    "remaining_before_remediation_seconds": 90.0,
                    "remediation_timeout_seconds": 90.0,
                    "schema_version": 1,
                    "session_id": "session",
                },
                "detected",
            )
            checkout = root / "checkout"
            checkout.mkdir()
            with patch("testinfra.tfa.pipeline.run_process") as process:
                for _invocation in range(2):
                    with self.assertRaisesRegex(RuntimeError, "explicit reset"):
                        diagnose_candidate(
                            case,
                            case_dir,
                            checkout,
                            root / "report.schema.json",
                            120,
                        )
            process.assert_not_called()
            self.assertEqual(
                json.loads((attempt / "remediation.json").read_text())["phase"],
                "detected",
            )

    def test_candidate_apply_refuses_without_exact_preapproval(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            case_dir = root / "artifacts" / case.case_id
            output = case_dir / "agent-output"
            output.mkdir(parents=True)
            ReportTest._write_artifacts(output)
            (output / "patch.diff").write_text(
                "diff --git a/fbcode/product/fix.cpp b/fbcode/product/fix.cpp\n"
                "new file mode 100644\n--- /dev/null\n"
                "+++ b/fbcode/product/fix.cpp\n@@ -0,0 +1 @@\n+fixed\n"
            )
            report = output / "report.json"
            report.write_text(json.dumps(_valid_report(case.case_id, output)))
            attempt = case_dir / "control" / "candidate-attempts" / "attempt-001"
            attempt.mkdir(parents=True)
            candidate = seal_candidate_report(
                report, attempt / "candidate-seal.json", case.case_id
            )
            checkout = root / "checkout"
            checkout.mkdir()
            with (
                patch("testinfra.tfa.pipeline.run_process") as process,
                patch("testinfra.tfa.pipeline._checkout_state") as checkout_state,
                self.assertRaisesRegex(RuntimeError, "no approved exact diff"),
            ):
                pipeline_module._apply_candidate_locked(
                    case, case_dir, checkout, attempt, candidate
                )
            process.assert_not_called()
            checkout_state.assert_not_called()
            self.assertFalse((checkout / "fbcode/product/fix.cpp").exists())

    def test_candidate_approval_copy_crash_leaves_durable_blocker(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            case_dir = root / "artifacts" / case.case_id
            output = case_dir / "agent-output"
            output.mkdir(parents=True)
            ReportTest._write_artifacts(output)
            (output / "patch.diff").write_text(
                "diff --git a/fbcode/product/fix.cpp b/fbcode/product/fix.cpp\n"
                "new file mode 100644\n--- /dev/null\n"
                "+++ b/fbcode/product/fix.cpp\n@@ -0,0 +1 @@\n+fixed\n"
            )
            report = output / "report.json"
            report.write_text(json.dumps(_valid_report(case.case_id, output)))
            attempt = case_dir / "control" / "candidate-attempts" / "attempt-001"
            attempt.mkdir(parents=True)
            seal_candidate_report(report, attempt / "candidate-seal.json", case.case_id)
            checkout = root / "checkout"
            checkout.mkdir()
            with (
                patch(
                    "testinfra.tfa.pipeline._write_control_blob",
                    side_effect=OSError("injected copy crash"),
                ),
                self.assertRaisesRegex(OSError, "injected copy crash"),
            ):
                pipeline_module._approve_candidate_locked(
                    case,
                    case_dir,
                    checkout,
                    attempt,
                    root / "review.schema.json",
                )
            transaction = json.loads((attempt / "pre-apply-review.json").read_text())
            self.assertEqual(transaction["phase"], "pinning")
            with (
                patch("testinfra.tfa.pipeline._run_candidate_review") as review,
                self.assertRaisesRegex(RuntimeError, "explicit reset required"),
            ):
                pipeline_module._approve_candidate_locked(
                    case,
                    case_dir,
                    checkout,
                    attempt,
                    root / "review.schema.json",
                )
            review.assert_not_called()

    def test_candidate_apply_refuses_approved_hash_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            case_dir = root / "artifacts" / case.case_id
            output = case_dir / "agent-output"
            output.mkdir(parents=True)
            ReportTest._write_artifacts(output)
            patch_path = output / "patch.diff"
            patch_path.write_text(
                "diff --git a/fbcode/product/fix.cpp b/fbcode/product/fix.cpp\n"
                "new file mode 100644\n--- /dev/null\n"
                "+++ b/fbcode/product/fix.cpp\n@@ -0,0 +1 @@\n+fixed\n"
            )
            report = output / "report.json"
            report.write_text(json.dumps(_valid_report(case.case_id, output)))
            attempt = case_dir / "control" / "candidate-attempts" / "attempt-001"
            attempt.mkdir(parents=True)
            candidate = seal_candidate_report(
                report, attempt / "candidate-seal.json", case.case_id
            )
            checkout = root / "checkout"
            checkout.mkdir()

            def approve(
                _checkout: Path,
                changed: tuple[str, ...],
                _case_dir: Path,
                _attempt: Path,
                approved_patch: Path,
                diff_sha256: str,
                _schema: Path,
            ) -> dict[str, object]:
                reviewer = root / "reviewer"
                reviewer.mkdir()
                review = reviewer / "review.json"
                review.write_text(
                    json.dumps(
                        {
                            "changed_paths": list(changed),
                            "diff_sha256": diff_sha256,
                            "findings": [],
                            "goalpost_assessment": _approving_goalpost_assessment(),
                            "no_goalposts_moved": True,
                            "schema_version": 1,
                            "summary": "Exact candidate diff is approved.",
                            "verdict": "APPROVED",
                        }
                    )
                )
                return {
                    "agent_patch": regular_file_record(patch_path),
                    "approved_patch": regular_file_record(approved_patch),
                    "attempt": str(attempt),
                    "candidate_seal": regular_file_record(
                        attempt / "candidate-seal.json"
                    ),
                    "changed_paths": list(changed),
                    "diff_sha256": diff_sha256,
                    "process_argv": ["/usr/local/bin/codex", "--version"],
                    "process_dir": str(root / "review-process"),
                    "review": regular_file_record(review),
                    "schema_version": 1,
                    "summary": "Exact candidate diff is approved.",
                }

            with (
                patch(
                    "testinfra.tfa.pipeline._run_candidate_review",
                    side_effect=approve,
                ),
                patch("testinfra.tfa.pipeline.recover_completed_process"),
            ):
                pipeline_module._approve_candidate_locked(
                    case,
                    case_dir,
                    checkout,
                    attempt,
                    root / "review.schema.json",
                )
            review_transaction = attempt / "pre-apply-review.json"
            transaction = json.loads(review_transaction.read_text())
            transaction["changed_paths"] = ["fbcode/product/unreviewed.cpp"]
            write_metadata(review_transaction, transaction)
            with (
                patch("testinfra.tfa.pipeline.run_process") as process,
                patch("testinfra.tfa.pipeline._checkout_state") as checkout_state,
                patch("testinfra.tfa.pipeline.recover_completed_process"),
                self.assertRaisesRegex(
                    RuntimeError, "review journal differs from approval"
                ),
            ):
                pipeline_module._apply_candidate_locked(
                    case, case_dir, checkout, attempt, candidate
                )
            process.assert_not_called()
            checkout_state.assert_not_called()
            transaction["changed_paths"] = ["fbcode/product/fix.cpp"]
            write_metadata(review_transaction, transaction)
            patch_path.write_text(patch_path.read_text() + "# mutation\n")
            with (
                patch("testinfra.tfa.pipeline.run_process") as process,
                patch("testinfra.tfa.pipeline._checkout_state") as checkout_state,
                patch("testinfra.tfa.pipeline.recover_completed_process"),
                self.assertRaises(StateError),
            ):
                pipeline_module._apply_candidate_locked(
                    case, case_dir, checkout, attempt, candidate
                )
            process.assert_not_called()
            checkout_state.assert_not_called()
            self.assertFalse((checkout / "fbcode/product/fix.cpp").exists())

    def test_validation_review_rejection_runs_no_candidate_commands(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            checkout = root / "checkout"
            source = checkout / "fbcode/product/fix.cpp"
            source.parent.mkdir(parents=True)
            source.write_text("fixed\n")
            with (
                patch("testinfra.tfa.pipeline._assert_original_artifact"),
                patch(
                    "testinfra.tfa.pipeline.validate_witness_artifacts",
                    return_value={"sealed": True},
                ),
                patch("testinfra.tfa.pipeline._json", return_value={"sealed": True}),
                patch(
                    "testinfra.tfa.pipeline._checkout_state",
                    return_value=("fbcode/product/fix.cpp",),
                ),
                patch(
                    "testinfra.tfa.pipeline._independent_diff_review",
                    side_effect=RuntimeError("review rejected"),
                ),
                patch("testinfra.tfa.pipeline.run_process") as process,
                self.assertRaisesRegex(RuntimeError, "review rejected"),
            ):
                independent_validate(
                    case,
                    root / "artifacts" / case.case_id,
                    checkout,
                    root / "repro",
                    root / "review.schema.json",
                )
            process.assert_not_called()

    def test_killed_applied_diff_reviewer_runs_no_candidate_commands(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case = _loaded_case()
            checkout = root / "checkout"
            source = checkout / "fbcode/product/fix.cpp"
            source.parent.mkdir(parents=True)
            source.write_text("fixed\n")
            diff_stdout = root / "diff.stdout"
            diff_stderr = root / "diff.stderr"
            diff_stdout.write_text(
                "diff --git a/fbcode/product/fix.cpp b/fbcode/product/fix.cpp\n"
            )
            diff_stderr.write_text("")
            diff = ProcessResult(
                ("/usr/local/bin/sl", "diff"),
                0,
                0.1,
                False,
                diff_stdout,
                diff_stderr,
            )
            review_stdout = root / "review.stdout"
            review_stderr = root / "review.stderr"
            review_stdout.write_text("")
            review_stderr.write_text("reviewer killed\n")
            killed = ProcessResult(
                ("/usr/local/bin/codex", "exec"),
                -9,
                120.0,
                True,
                review_stdout,
                review_stderr,
            )
            case_dir = root / "artifacts" / case.case_id

            def capture_diff(
                _checkout: Path,
                _changed: tuple[str, ...],
                artifact_dir: Path,
            ) -> ProcessResult:
                artifact_dir.mkdir(parents=True)
                return diff

            with (
                patch("testinfra.tfa.pipeline._assert_original_artifact"),
                patch(
                    "testinfra.tfa.pipeline.validate_witness_artifacts",
                    return_value={"sealed": True},
                ),
                patch("testinfra.tfa.pipeline._json", return_value={"sealed": True}),
                patch(
                    "testinfra.tfa.pipeline._checkout_state",
                    return_value=("fbcode/product/fix.cpp",),
                ),
                patch("testinfra.tfa.pipeline._sl_diff", side_effect=capture_diff),
                patch(
                    "testinfra.tfa.pipeline.run_process", return_value=killed
                ) as process,
                self.assertRaisesRegex(
                    RuntimeError, "independent diff reviewer did not complete"
                ),
            ):
                independent_validate(
                    case,
                    case_dir,
                    checkout,
                    root / "repro",
                    root / "review.schema.json",
                )
            process.assert_called_once()
            self.assertEqual(process.call_args.args[0], "/usr/local/bin/codex")

    def test_draft_safety_readback_uses_fresh_artifacts_on_every_resume(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            case_dir = root / "case"
            control_dir = case_dir / "control"
            before = root / "before.diff"
            after = root / "after.diff"
            before.write_text("diff\n")
            after.write_text("diff\n")
            diff_sha256 = hashlib.sha256(before.read_bytes()).hexdigest()
            write_metadata(control_dir / "diff-url.json", {"number": "D123"})
            write_metadata(
                control_dir / "validation-seal.json",
                {
                    "changed_paths": ["file.cpp"],
                    "diff_after_file_capture": regular_file_record(after),
                    "diff_before_file_capture": regular_file_record(before),
                    "diff_sha256": diff_sha256,
                    "files": [],
                },
            )
            result = DraftReadback(
                "https://www.internalfb.com/diff/123",
                "D123",
                "commit",
                ProcessResult(("meta",), 0, 0, False, root / "out", root / "err"),
                ProcessResult(("meta",), 0, 0, False, root / "out", root / "err"),
            )
            with patch(
                "testinfra.tfa.pipeline.readback_draft", return_value=result
            ) as readback:
                _revalidate_draft(_loaded_case(), case_dir, root / "checkout")
                _revalidate_draft(_loaded_case(), case_dir, root / "checkout")
            self.assertEqual(
                [call.args[1].name for call in readback.call_args_list],
                ["attempt-001", "attempt-002"],
            )

    def test_thrift_draft_resume_requires_final_commit_seal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            case_dir = root / "case"
            control_dir = case_dir / "control"
            control_dir.mkdir(parents=True)
            case = replace(_loaded_case(), case_id="thrift")
            canonical, mirrored = pipeline_module._DECLARED_CASE_MIRRORS["thrift"]
            for relative in (canonical, mirrored):
                path = checkout / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("same changed bytes\n")
            qualified_paths = (canonical,)
            qualified_files = [pipeline_module._path_record(checkout, canonical)]
            final_paths = pipeline_module._declared_draft_paths(case, qualified_paths)
            final_files = pipeline_module._tree_records(checkout, final_paths)
            commit_stdout = root / "commit.stdout"
            commit_stdout.write_text(
                f"mirrored changes in {canonical!r} to {mirrored!r}\n"
            )
            write_metadata(control_dir / "diff-url.json", {"number": "D123"})
            write_metadata(
                control_dir / "validation-seal.json",
                {
                    "changed_paths": list(qualified_paths),
                    "files": qualified_files,
                },
            )
            write_metadata(
                control_dir / "draft-transaction.json",
                {
                    "commit_stdout": regular_file_record(commit_stdout),
                    "final_changed_paths": list(final_paths),
                    "final_files": final_files,
                },
            )
            stdout = root / "stdout.txt"
            stderr = root / "stderr.txt"
            stdout.write_text("")
            stderr.write_text("")
            readback_result = DraftReadback(
                "https://www.internalfb.com/diff/123",
                "D123",
                "commit",
                ProcessResult(("meta",), 0, 0, False, stdout, stderr),
                ProcessResult(("meta",), 0, 0, False, stdout, stderr),
            )
            with (
                patch("testinfra.tfa.pipeline._seal_thrift_final_commit") as seal,
                patch("testinfra.tfa.pipeline.readback_draft") as readback,
                self.assertRaisesRegex(
                    RuntimeError, "final mirror commit seal is missing"
                ),
            ):
                _revalidate_draft(case, case_dir, checkout)
            seal.assert_not_called()
            readback.assert_not_called()

            (control_dir / "final-commit-seal.json").write_text("{}\n")
            with (
                patch(
                    "testinfra.tfa.pipeline._seal_thrift_final_commit",
                    return_value=(final_paths, final_files),
                ) as seal,
                patch(
                    "testinfra.tfa.pipeline.readback_draft",
                    return_value=readback_result,
                ) as readback,
            ):
                self.assertEqual(
                    _revalidate_draft(case, case_dir, checkout),
                    "https://www.internalfb.com/diff/123",
                )
            seal.assert_called_once()
            self.assertIsNone(seal.call_args.args[6])
            readback.assert_called_once()

    def test_thrift_final_commit_seals_declared_mirror_and_two_path_review(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            case_dir = root / "case"
            canonical, mirrored = pipeline_module._DECLARED_CASE_MIRRORS["thrift"]
            for relative in (canonical, mirrored):
                path = checkout / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("same changed bytes\n")
            qualified_paths = (canonical,)
            qualified_files = [pipeline_module._path_record(checkout, canonical)]
            commit_stdout = root / "commit.stdout"
            mirror_message = f"mirrored changes in {canonical!r} to {mirrored!r}"
            commit_stdout.write_text(mirror_message + "\n")
            reviewed_paths: list[tuple[str, ...]] = []

            def diff(
                _checkout: Path,
                paths: tuple[str, ...],
                artifact_dir: Path,
                _base: str | None = None,
            ) -> ProcessResult:
                artifact_dir.mkdir(parents=True)
                stdout = artifact_dir / "stdout.txt"
                stderr = artifact_dir / "stderr.txt"
                path = paths[0]
                stdout.write_text(
                    f"diff --git a/{path} b/{path}\n"
                    f"--- a/{path}\n+++ b/{path}\n"
                    "@@ -1 +1 @@\n-old\n+same changed bytes\n"
                )
                stderr.write_text("")
                return ProcessResult(("sl", "diff"), 0, 0.1, False, stdout, stderr)

            def review(
                _checkout: Path,
                paths: tuple[str, ...],
                directory: Path,
                _schema: Path,
                **_kwargs: object,
            ) -> dict[str, object]:
                reviewed_paths.append(paths)
                review_path = directory / "control" / "final-adversarial-review.json"
                review_path.parent.mkdir(parents=True, exist_ok=True)
                review_path.write_text("{}\n")
                return {"diff_sha256": "a" * 64}

            case = replace(_loaded_case(), case_id="thrift")
            with (
                patch("testinfra.tfa.pipeline._sl_diff", side_effect=diff),
                patch(
                    "testinfra.tfa.pipeline._independent_diff_review",
                    side_effect=review,
                ),
            ):
                paths, files = pipeline_module._seal_thrift_final_commit(
                    case,
                    case_dir,
                    checkout,
                    qualified_paths,
                    qualified_files,
                    commit_stdout,
                    root / "review.schema.json",
                )
            self.assertEqual(paths, tuple(sorted((canonical, mirrored))))
            self.assertEqual(reviewed_paths, [paths])
            self.assertEqual(files[0]["sha256"], files[1]["sha256"])
            seal = json.loads(
                (case_dir / "control" / "final-commit-seal.json").read_text()
            )
            self.assertEqual(seal["changed_paths"], list(paths))

            with (
                patch(
                    "testinfra.tfa.pipeline._verify_independent_diff_review"
                ) as verify_review,
                patch(
                    "testinfra.tfa.pipeline._independent_diff_review"
                ) as create_review,
            ):
                resumed_paths, resumed_files = (
                    pipeline_module._seal_thrift_final_commit(
                        case,
                        case_dir,
                        checkout,
                        qualified_paths,
                        qualified_files,
                        commit_stdout,
                        None,
                    )
                )
            self.assertEqual((resumed_paths, resumed_files), (paths, files))
            verify_review.assert_called_once()
            create_review.assert_not_called()

            seal_path = case_dir / "control" / "final-commit-seal.json"
            held_seal = seal_path.with_name("held-final-commit-seal.json")
            seal_path.replace(held_seal)
            try:
                with (
                    patch("testinfra.tfa.pipeline._sl_diff") as diff,
                    patch(
                        "testinfra.tfa.pipeline._independent_diff_review"
                    ) as create_review,
                    self.assertRaisesRegex(
                        RuntimeError, "final mirror commit seal is missing"
                    ),
                ):
                    pipeline_module._seal_thrift_final_commit(
                        case,
                        case_dir,
                        checkout,
                        qualified_paths,
                        qualified_files,
                        commit_stdout,
                        None,
                    )
                diff.assert_not_called()
                create_review.assert_not_called()
            finally:
                held_seal.replace(seal_path)

            commit_stdout.write_text(
                commit_stdout.read_text()
                + "mirrored changes in 'other' to 'unrelated'\n"
            )
            (case_dir / "control" / "final-commit-seal.json").unlink()
            with self.assertRaisesRegex(RuntimeError, "declared mirror pair"):
                pipeline_module._seal_thrift_final_commit(
                    case,
                    case_dir,
                    checkout,
                    qualified_paths,
                    qualified_files,
                    commit_stdout,
                    root / "review.schema.json",
                )

    def test_thrift_final_commit_rejects_divergent_mirror(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            canonical, mirrored = pipeline_module._DECLARED_CASE_MIRRORS["thrift"]
            for relative, content in (
                (canonical, "canonical\n"),
                (mirrored, "other\n"),
            ):
                path = checkout / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            stdout = root / "commit.stdout"
            mirror_message = f"mirrored changes in {canonical!r} to {mirrored!r}"
            stdout.write_text(mirror_message + "\n")
            case = replace(_loaded_case(), case_id="thrift")
            with self.assertRaisesRegex(RuntimeError, "canonical and mirror files"):
                pipeline_module._seal_thrift_final_commit(
                    case,
                    root / "case",
                    checkout,
                    (canonical,),
                    [pipeline_module._path_record(checkout, canonical)],
                    stdout,
                    root / "review.schema.json",
                )

    def test_independent_review_is_bound_to_exact_diff_hash_and_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prompt = render_review_prompt(
                root / "checkout",
                root / "diff",
                "a" * 64,
                ("test.cpp",),
            )
            goalpost_block = (
                "GOALPOST-MOVING REVIEW RULE\n\n"
                "Adversarial reviewers must look explicitly for goalpost moving. "
                "We are extremely skeptical of any goalpost moving. YOU DO NOT "
                "CLEAR THE BAR BY SIMPLY LOWERING THE BAR.\n\n"
                "Treat each of these as an explicit review target:\n"
                "- weakening an assertion so a test passes\n"
                "- widening a tolerance · adding an exemption · skipping a case · "
                "relaxing a comparator\n"
                "- renaming or relabelling so a failure reads as a pass\n"
                "- deleting a check rather than satisfying it\n\n"
                "Compare every changed test, threshold, allowlist, skip, label, "
                "classification, and gate with the base revision. Determine whether "
                "the implementation now meets the original requirement or whether "
                "the requirement was changed to admit the implementation. Call out "
                "either result explicitly, including when no lowering of the bar is "
                "present."
            )
            self.assertIn(goalpost_block, prompt)
            self.assertIn("deleted or removed coverage", prompt)
            review = root / "review.json"
            review.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "diff_sha256": "a" * 64,
                        "changed_paths": ["test.cpp"],
                        "verdict": "APPROVED",
                        "goalpost_assessment": _approving_goalpost_assessment(),
                        "no_goalposts_moved": True,
                        "summary": "Exact diff preserves every evidence goalpost.",
                        "findings": [],
                    }
                )
            )
            validate_review(review, "a" * 64, ("test.cpp",))
            with self.assertRaisesRegex(ReviewError, "different diff"):
                validate_review(review, "b" * 64, ("test.cpp",))
            with self.assertRaisesRegex(ReviewError, "different paths"):
                validate_review(review, "a" * 64, ("helper.cpp",))
            valid = json.loads(review.read_text())
            for name, invalid in (
                ("missing", None),
                ("false", False),
                (
                    "malformed",
                    {
                        **_approving_goalpost_assessment(),
                        "deleted_check_coverage": "UNKNOWN",
                    },
                ),
                (
                    "weakening-found",
                    {
                        **_approving_goalpost_assessment(),
                        "assertion_weakening": "FOUND",
                    },
                ),
            ):
                with self.subTest(name=name):
                    rejected = dict(valid)
                    if invalid is None:
                        rejected.pop("goalpost_assessment")
                    else:
                        rejected["goalpost_assessment"] = invalid
                    review.write_text(json.dumps(rejected))
                    with self.assertRaisesRegex(
                        ReviewError, "structured goalpost assessment"
                    ):
                        validate_review(review, "a" * 64, ("test.cpp",))

    def test_validation_seal_rejects_mutation_between_file_capture_and_rediff(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            changed_path = "fbcode/product/fix.cpp"
            source = checkout / changed_path
            source.parent.mkdir(parents=True)
            source.write_text("reviewed\n")
            reviewed = root / "reviewed.diff"
            reviewed.write_text("reviewed diff\n")
            mutated = root / "mutated.diff"
            mutated.write_text("mutated diff\n")
            reviewed_hash = hashlib.sha256(reviewed.read_bytes()).hexdigest()
            calls = 0

            def rediff(*_args: object) -> ProcessResult:
                nonlocal calls
                calls += 1
                if calls == 1:
                    return ProcessResult(("sl",), 0, 0, False, reviewed, root / "err-1")
                source.write_text("mutated\n")
                return ProcessResult(("sl",), 0, 0, False, mutated, root / "err-2")

            with (
                patch("testinfra.tfa.pipeline._sl_diff", side_effect=rediff),
                self.assertRaisesRegex(RuntimeError, "during file-record capture"),
            ):
                _write_validation_seal(
                    _loaded_case(),
                    checkout,
                    (changed_path,),
                    root / "case",
                    root / "capture",
                    reviewed_hash,
                )
            self.assertEqual(source.read_text(), "mutated\n")
            self.assertFalse(
                (root / "case" / "control" / "validation-seal.json").exists()
            )

    def test_recovery_counts_only_complete_exact_fresh_rows(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "checkout"
            changed_path = "fbcode/product/fix.cpp"
            changed = checkout / changed_path
            changed.parent.mkdir(parents=True)
            changed.write_text("fixed\n")
            working = root / "cwd"
            case_dir = root / "case"
            case_dir.mkdir()
            control_dir = case_dir / "control"
            base_case = _loaded_case()
            pass_argv = tuple(
                token.replace(base_case.working_directory, str(working))
                for token in base_case.pass_argv
            )
            case = replace(
                base_case,
                pass_argv=pass_argv,
                working_directory=str(working),
            )
            seal_working_directory(working, control_dir / "working-directory-seal.json")
            before = root / "before.diff"
            after = root / "after.diff"
            before.write_text("diff\n")
            after.write_text("diff\n")
            diff_sha256 = hashlib.sha256(before.read_bytes()).hexdigest()
            write_metadata(
                control_dir / "validation-seal.json",
                {
                    "base_revision": case.source_revision,
                    "changed_paths": [changed_path],
                    "diff_after_file_capture": regular_file_record(after),
                    "diff_before_file_capture": regular_file_record(before),
                    "diff_sha256": diff_sha256,
                    "files": [
                        {
                            "kind": "file",
                            "mode": changed.stat().st_mode & 0o7777,
                            "path": changed_path,
                            "sha256": hashlib.sha256(changed.read_bytes()).hexdigest(),
                            "size": changed.stat().st_size,
                        }
                    ],
                },
            )
            attempt = control_dir / "validation-attempts" / "attempt-001"
            elf = checkout / "buck-out" / "test-elf"
            elf.parent.mkdir()
            elf.write_bytes(b"elf")
            source_tree = elf.parent / f"{elf.parent.name}shared_libs_symlink_tree"
            source_tree.mkdir()
            external_source = root / "external" / "libexternal.so"
            external_source.parent.mkdir()
            external_source.write_bytes(b"runtime")
            external_link = source_tree / "libexternal.so"
            external_link.symlink_to(external_source)
            internal_target = elf.parent / "libinternal-target.so"
            internal_target.write_bytes(b"internal")
            internal_link = source_tree / "libinternal.so"
            internal_link.symlink_to(internal_target)
            runtime_path = attempt / "runtime-materialization.json"
            runtime_inventory = pipeline_module._materialize_external_runtime_links(
                elf, elf.parent, runtime_path
            )
            runtime_elf_record = runtime_inventory["runtime_elf"]
            self.assertIsInstance(runtime_elf_record, dict)
            runtime_elf = Path(str(runtime_elf_record["path"]))
            runtime_tree = Path(str(runtime_inventory["tree"]))
            runtime_file = runtime_tree / external_link.name
            runtime_internal_link = runtime_tree / internal_link.name
            materialized = runtime_inventory["materialized"]
            self.assertIsInstance(materialized, list)
            runtime_record = materialized[0]
            runtime_entries = runtime_inventory["entries"]
            self.assertIsInstance(runtime_entries, list)
            external_runtime_entry = next(
                value for value in runtime_entries if value["external"] is True
            )
            rows: list[dict[str, object]] = []
            seeds = (case.fail_seed, case.pass_seed, *case.fresh_stress_seeds)
            for index, seed in enumerate(seeds):
                row_root = attempt / f"seed-{seed}"
                row_root.mkdir(parents=True)
                controller = row_root / "controller-cwd"
                controller.mkdir(mode=0o700)
                seal_working_directory(
                    controller, row_root / "controller-cwd.before.json"
                )
                seal_directory_evidence(
                    controller, row_root / "controller-cwd.after.json"
                )
                wrapper = row_root / "wrapper"
                wrapper.mkdir()
                stdout = wrapper / "stdout.txt"
                report = row_root / "safehermit.txt"
                validation_argv = hermit_attempt_argv(
                    pipeline_module._validation_base_argv(
                        case, seed, runtime_elf, row_root
                    ),
                    Path(case.working_directory),
                    row_root / "controller-cwd",
                    180,
                    safehermit_wrapper_path(row_root),
                )
                safehermit_metadata, validation_argv = _write_safehermit_fixture(
                    row_root, validation_argv, 0, case.hermit_sha256
                )
                identity = pipeline_module._gtest_identity(validation_argv)
                stdout.write_text(
                    f"Note: Google Test filter = {identity}\n"
                    "[==========] Running 1 test from 1 test suite.\n"
                    f"[ RUN      ] {identity}\n"
                    f"[       OK ] {identity} (1 ms)\n"
                    "[  PASSED  ] 1 test.\n"
                )
                artifact_paths = (
                    stdout,
                    report,
                    row_root / "execution-inputs.json",
                    *safehermit_runtime_paths(row_root),
                )
                rows.append(
                    {
                        "argv": list(validation_argv),
                        "artifacts": [str(path) for path in artifact_paths],
                        "artifact_records": [
                            regular_file_record(path) for path in artifact_paths
                        ],
                        "complete": True,
                        "controller_evidence": controller_evidence(row_root),
                        "gtest_execution": (
                            pipeline_module._gtest_execution_evidence(
                                validation_argv, stdout, True
                            )
                        ),
                        "kind": (
                            "ORIGINAL_FAIL"
                            if index == 0
                            else ("ORIGINAL_PASS" if index == 1 else "FRESH_STRESS")
                        ),
                        "passed": 1,
                        "safehermit": safehermit_evidence(
                            validation_argv,
                            safehermit_metadata,
                            report,
                            case.hermit_sha256,
                        ),
                        "status": "PASS",
                        "total": 1,
                    }
                )
            buck_stdout = attempt / "buck.stdout"
            buck_stderr = attempt / "buck.stderr"
            lint_stdout = attempt / "lint.stdout"
            lint_stderr = attempt / "lint.stderr"
            for artifact in (buck_stdout, buck_stderr, lint_stdout, lint_stderr):
                artifact.write_text("pass")
            transaction_path = runtime_path.with_name(
                "runtime-materialization-transaction.json"
            )

            def write_runtime_inventory() -> None:
                write_metadata(runtime_path, runtime_inventory)
                transaction = json.loads(transaction_path.read_text())
                transaction["inventory_record"] = regular_file_record(runtime_path)
                write_metadata(transaction_path, transaction)

            def write_validation() -> None:
                write_metadata(
                    control_dir / "validation.json",
                    {
                        "attempt": str(attempt),
                        "build": {
                            "artifact": str(runtime_elf),
                            "artifact_record": regular_file_record(runtime_elf),
                            "artifact_sha256": hashlib.sha256(
                                runtime_elf.read_bytes()
                            ).hexdigest(),
                            "source_artifact": str(elf),
                            "source_artifact_record": regular_file_record(elf),
                        },
                        "buck_test": {
                            "status": "PASS",
                            "artifacts": [str(buck_stdout), str(buck_stderr)],
                            "artifact_records": [
                                regular_file_record(buck_stdout),
                                regular_file_record(buck_stderr),
                            ],
                        },
                        "lint": {
                            "status": "PASS",
                            "artifacts": [str(lint_stdout), str(lint_stderr)],
                            "artifact_records": [
                                regular_file_record(lint_stdout),
                                regular_file_record(lint_stderr),
                            ],
                        },
                        "runtime_materialization": {
                            "artifact": regular_file_record(runtime_path),
                            "transaction": regular_file_record(
                                runtime_path.with_name(
                                    "runtime-materialization-transaction.json"
                                )
                            ),
                            **runtime_inventory,
                        },
                        "validations": rows,
                    },
                )

            write_validation()
            with patch("testinfra.tfa.pipeline._verify_independent_diff_review"):
                validation = _validation_result(case, case_dir, checkout)
            self.assertEqual(validation.stress_passed, len(case.fresh_stress_seeds))
            transaction = json.loads(transaction_path.read_text())
            expected_runtime_root = transaction["runtime_root"]
            expected_staging = transaction["staging"]
            expected_preserved = transaction["preserved_staging"]
            transaction["runtime_root"] = str(root / "contradictory-runtime-root")
            write_metadata(transaction_path, transaction)
            write_validation()
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "transaction is incomplete"),
            ):
                _validation_result(case, case_dir, checkout)
            transaction["runtime_root"] = expected_runtime_root
            transaction["staging"] = str(root / "escaped.staging")
            write_metadata(transaction_path, transaction)
            write_validation()
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "transaction is incomplete"),
            ):
                _validation_result(case, case_dir, checkout)
            transaction["staging"] = expected_staging
            transaction["preserved_staging"] = {}
            write_metadata(transaction_path, transaction)
            write_validation()
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "staging is malformed"),
            ):
                _validation_result(case, case_dir, checkout)
            transaction = json.loads(
                runtime_path.with_name(
                    "runtime-materialization-transaction.json"
                ).read_text()
            )
            transaction["runtime_root"] = expected_runtime_root
            transaction["staging"] = expected_staging
            transaction["preserved_staging"] = expected_preserved
            write_metadata(transaction_path, transaction)
            write_validation()
            runtime_file.write_bytes(b"tampered")
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "closure differs"),
            ):
                _validation_result(case, case_dir, checkout)
            runtime_file.write_bytes(b"runtime")
            runtime_file.unlink()
            runtime_file.symlink_to(elf)
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "closure differs"),
            ):
                _validation_result(case, case_dir, checkout)
            runtime_file.unlink()
            runtime_file.write_bytes(b"runtime")
            external_source.write_bytes(b"tampered")
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(StateError, "differs from its seal"),
            ):
                _validation_result(case, case_dir, checkout)
            external_source.write_bytes(b"runtime")
            external_source.chmod(0o600)
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(StateError, "differs from its seal"),
            ):
                _validation_result(case, case_dir, checkout)
            external_source.chmod(0o644)
            external_source.unlink()
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(StateError, "cannot inspect evidence file"),
            ):
                _validation_result(case, case_dir, checkout)
            external_source.write_bytes(b"runtime")
            external_source.unlink()
            external_source.symlink_to(internal_target)
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(StateError, "not a regular file"),
            ):
                _validation_result(case, case_dir, checkout)
            external_source.unlink()
            external_source.write_bytes(b"different")
            changed_source_record = regular_file_record(external_source)
            runtime_record["source_record"] = changed_source_record
            external_runtime_entry["source"] = changed_source_record
            write_runtime_inventory()
            write_validation()
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(
                    RuntimeError, "source differs from materialized file"
                ),
            ):
                _validation_result(case, case_dir, checkout)
            external_source.write_bytes(b"runtime")
            restored_source_record = regular_file_record(external_source)
            runtime_record["source_record"] = restored_source_record
            external_runtime_entry["source"] = restored_source_record
            write_runtime_inventory()
            write_validation()
            internal_target.write_bytes(b"tampered")
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "closure differs"),
            ):
                _validation_result(case, case_dir, checkout)
            internal_target.write_bytes(b"internal")
            runtime_internal_link.unlink()
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "closure differs"),
            ):
                _validation_result(case, case_dir, checkout)
            runtime_internal_link.symlink_to(internal_target)
            runtime_internal_link.unlink()
            runtime_internal_link.symlink_to(external_source)
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "escaped buck-out"),
            ):
                _validation_result(case, case_dir, checkout)
            runtime_internal_link.unlink()
            runtime_internal_link.symlink_to(internal_target)
            runtime_elf.write_bytes(b"tampered")
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(StateError, "differs from its seal"),
            ):
                _validation_result(case, case_dir, checkout)
            runtime_elf.write_bytes(b"elf")
            elf.write_bytes(b"tampered")
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(StateError, "differs from its seal"),
            ):
                _validation_result(case, case_dir, checkout)
            elf.write_bytes(b"elf")
            write_validation()
            last_artifacts = rows[-1]["artifacts"]
            self.assertIsInstance(last_artifacts, list)
            last_report = Path(str(last_artifacts[1]))
            last_report_bytes = last_report.read_bytes()
            last_report.write_text("tampered")
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(StateError, "differs from its seal"),
            ):
                _validation_result(case, case_dir, checkout)
            last_report.write_bytes(last_report_bytes)
            rows[-1]["artifact_records"] = [
                regular_file_record(Path(str(path))) for path in last_artifacts
            ]
            rows[-1]["complete"] = False
            write_validation()
            with (
                patch("testinfra.tfa.pipeline._verify_independent_diff_review"),
                self.assertRaisesRegex(RuntimeError, "complete exact PASS"),
            ):
                _validation_result(case, case_dir, checkout)

    def test_failure_class_uses_stdout_and_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "wrapper"
            wrapper.mkdir()
            (wrapper / "stdout.txt").write_text(
                "F0921 HTTPDownstreamCoroSessionTests.cpp:1685\n"
            )
            (wrapper / "stderr.txt").write_text(
                "Check failed: bodyEvent.hasException()\n"
            )
            case = replace(_loaded_case(), case_id="httpcoro")
            self.assertTrue(_matches_failure_class(case, root))

    def test_independent_validation_refuses_modified_recorded_elf(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            guest = root / "buck-out" / "test-elf"
            guest.parent.mkdir()
            guest.write_bytes(b"unexpected replacement")
            case = _loaded_case()
            argv = list(case.pass_argv)
            argv[argv.index("--") + 1] = str(guest)
            case = replace(
                case,
                pass_argv=tuple(argv),
                artifact_sha256="0" * 64,
            )
            with (
                patch("testinfra.tfa.pipeline._checkout_state") as checkout_state,
                self.assertRaisesRegex(RuntimeError, "target ELF hash differs"),
            ):
                independent_validate(
                    case,
                    root / "artifacts",
                    root / "checkout",
                    root / "repro",
                    root / "review.schema.json",
                )
            checkout_state.assert_not_called()

    def test_independent_validation_blocks_draft_and_final_then_resumes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "case-checkout").mkdir()
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            manifest = load_manifest(manifest_path)
            case_dir = root / "artifacts" / "asyncssl"
            forged = case_dir / "agent-output" / "validation.json"
            forged.parent.mkdir(parents=True)
            forged.write_text('{"forged":true}\n')
            candidate = CandidateSummary(
                "asyncssl",
                "The originating invariant was violated.",
                str(case_dir / "agent-output" / "patch.diff"),
            )
            for name in ("report.json", "hypotheses.json", "patch.diff"):
                (case_dir / "agent-output" / name).write_text(name + "\n")
            diagnostic = case_dir / "control" / "diagnostic-agent"
            diagnostic.mkdir(parents=True)
            (diagnostic / "final-message.json").write_text("candidate\n")
            (diagnostic / "stdout.txt").write_text('{"thread_id":"session-1"}\n')
            write_metadata(
                case_dir / "control" / "active-candidate.json",
                {
                    "attempt": str(
                        case_dir / "control" / "candidate-attempts" / "attempt-001"
                    ),
                    "changed_paths": ["fbcode/product/fix.cpp"],
                    "handoff": {},
                },
            )
            summary = ReportSummary(
                "asyncssl",
                "The originating invariant was violated.",
                4,
                4,
                str(case_dir / "patch.diff"),
            )

            def reject(*_args: object) -> None:
                forged.unlink()
                (case_dir / "control" / "active-candidate.json").unlink()

            with (
                patch(
                    "testinfra.tfa.pipeline.validate_witness_artifacts",
                    return_value={"pass": {}, "fail": {}},
                ),
                patch(
                    "testinfra.tfa.pipeline._comment_stage",
                    return_value={"state": "authorization-blocked"},
                ) as comment,
                patch("testinfra.tfa.pipeline._checkout_state", return_value=()),
                patch(
                    "testinfra.tfa.pipeline._diagnose_candidate_locked",
                    return_value=candidate,
                ),
                patch(
                    "testinfra.tfa.pipeline._active_candidate",
                    return_value=candidate,
                ),
                patch(
                    "testinfra.tfa.pipeline.independent_validate",
                    side_effect=RuntimeError("independent validation failed"),
                ),
                patch(
                    "testinfra.tfa.pipeline._reject_active_candidate",
                    side_effect=reject,
                ) as reject_candidate,
                patch("testinfra.tfa.pipeline._draft") as draft,
                self.assertRaisesRegex(RuntimeError, "independent validation failed"),
            ):
                run_case(
                    manifest,
                    "asyncssl",
                    root / "artifacts",
                    root / "repro",
                    root / "case-checkout",
                    root / "schema.json",
                    None,
                    10,
                )
            self.assertEqual(
                [
                    entry.stage
                    for entry in Ledger(case_dir / "control" / "ledger.jsonl").read()
                ],
                [
                    Stage.VALIDATED_REPRO,
                    Stage.INITIAL_COMMENT,
                    Stage.INVESTIGATING,
                ],
            )
            self.assertEqual(comment.call_count, 1)
            reject_candidate.assert_called_once()
            draft.assert_not_called()
            self.assertFalse((case_dir / "control" / "validation.json").exists())
            self.assertFalse((case_dir / "control" / "active-candidate.json").exists())
            self.assertEqual(
                session_id_from_jsonl(diagnostic / "stdout.txt"), "session-1"
            )

            def validate(*_args: object) -> ValidationResult:
                return ValidationResult(("fbcode/product/fix.cpp",), 2, 2)

            def draft_fix(*_args: object) -> str:
                (case_dir / "control" / "diff-url.json").write_text(
                    json.dumps({"url": "https://www.internalfb.com/diff/123"})
                )
                write_metadata(
                    case_dir / "control" / "draft-transaction.json",
                    {"phase": "published"},
                )
                return "https://www.internalfb.com/diff/123"

            with (
                patch(
                    "testinfra.tfa.pipeline.validate_witness_artifacts",
                    return_value={"pass": {}, "fail": {}},
                ),
                patch(
                    "testinfra.tfa.pipeline.independent_validate",
                    side_effect=validate,
                ),
                patch(
                    "testinfra.tfa.pipeline._diagnose_candidate_locked",
                    return_value=candidate,
                ),
                patch(
                    "testinfra.tfa.pipeline._validation_result",
                    return_value=ValidationResult(("fbcode/product/fix.cpp",), 2, 2),
                ),
                patch(
                    "testinfra.tfa.pipeline._qualified_report",
                    return_value=summary,
                ),
                patch(
                    "testinfra.tfa.pipeline.validate_qualified_report",
                    return_value=summary,
                ),
                patch("testinfra.tfa.pipeline._draft", side_effect=draft_fix) as draft,
                patch(
                    "testinfra.tfa.pipeline._revalidate_draft",
                    return_value="https://www.internalfb.com/diff/123",
                ) as revalidate_draft,
                patch(
                    "testinfra.tfa.pipeline._comment_stage",
                    return_value={"state": "authorization-blocked"},
                ) as comment,
            ):
                run_case(
                    manifest,
                    "asyncssl",
                    root / "artifacts",
                    root / "repro",
                    root / "case-checkout",
                    root / "schema.json",
                    None,
                    10,
                )
            self.assertEqual(
                [
                    entry.stage
                    for entry in Ledger(case_dir / "control" / "ledger.jsonl").read()
                ],
                list(Stage),
            )
            draft.assert_called_once()
            self.assertEqual(revalidate_draft.call_count, 2)
            comment.assert_called_once()

    def test_run_case_recovers_published_draft_before_and_after_ledger(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / "case-checkout"
            checkout.mkdir()
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            manifest = load_manifest(manifest_path)
            case_dir = root / "artifacts" / "asyncssl"
            control = case_dir / "control"
            ledger = Ledger(control / "ledger.jsonl")
            for stage in (
                Stage.VALIDATED_REPRO,
                Stage.INITIAL_COMMENT,
                Stage.INVESTIGATING,
                Stage.FIX_DRAFTED,
                Stage.VERIFIED,
            ):
                ledger.append(stage, {"test": True})
            evidence: dict[str, object] = {"pass": {}, "fail": {}}
            write_metadata(control / "witness-seal.json", evidence)
            attempt = control / "candidate-attempts" / "attempt-001"
            attempt.mkdir(parents=True)
            write_metadata(attempt / "transaction.json", {"phase": "validating"})
            changed = ("fbcode/product/fix.cpp",)
            files = [{"kind": "file", "path": changed[0]}]
            write_metadata(
                control / "validation-seal.json",
                {"changed_paths": list(changed), "files": files},
            )
            write_metadata(
                control / "qualified-report.json",
                {
                    "candidate_transaction": regular_file_record(
                        attempt / "transaction.json"
                    )
                },
            )
            summary = ReportSummary(
                "asyncssl",
                "The originating invariant was violated.",
                4,
                4,
                str(case_dir / "patch.diff"),
            )
            url = "https://www.internalfb.com/diff/123"
            committed = False
            external_mutations = 0
            draft_calls = 0
            faults = ["committed", "submitted", "published"]
            original_write_transaction = pipeline_module._write_draft_transaction
            original_mark = pipeline_module._mark_draft_ledger_recorded

            def current_revision(
                _checkout: Path, _artifact_dir: Path, revision: str
            ) -> str:
                if revision == ".^":
                    return manifest.case("asyncssl").source_revision
                if committed:
                    return "committed-revision"
                return manifest.case("asyncssl").source_revision

            def commit_process(
                executable: str,
                arguments: tuple[str, ...],
                _cwd: Path,
                artifact_dir: Path,
                _timeout: float,
            ) -> ProcessResult:
                nonlocal committed
                self.assertEqual(executable, "/usr/local/bin/sl")
                self.assertEqual(arguments[0], "commit")
                committed = True
                artifact_dir.mkdir(parents=True)
                stdout = artifact_dir / "stdout.txt"
                stderr = artifact_dir / "stderr.txt"
                stdout.write_text("")
                stderr.write_text("")
                return ProcessResult(
                    (executable, *arguments), 0, 0.1, False, stdout, stderr
                )

            def draft_mutation(
                _checkout: Path,
                artifact_dir: Path,
                _base: str,
                _changed: tuple[str, ...],
                _files: list[dict[str, object]],
                _message: str,
            ) -> DraftMutation:
                nonlocal draft_calls, external_mutations
                draft_calls += 1
                if external_mutations == 0:
                    external_mutations += 1
                submit_dir = artifact_dir / "submit"
                describe_dir = artifact_dir / "readback" / "describe"
                files_dir = artifact_dir / "readback" / "files"
                for directory in (submit_dir, describe_dir, files_dir):
                    directory.mkdir(parents=True, exist_ok=True)
                    write_metadata(directory / "metadata.json", {"complete": True})
                submit = ProcessResult(
                    ("jf",),
                    0,
                    0.1,
                    False,
                    submit_dir / "stdout.txt",
                    submit_dir / "stderr.txt",
                )
                readback = DraftReadback(
                    url,
                    "D123",
                    "committed-revision",
                    ProcessResult(
                        ("meta",),
                        0,
                        0.1,
                        False,
                        describe_dir / "stdout.txt",
                        describe_dir / "stderr.txt",
                    ),
                    ProcessResult(
                        ("meta",),
                        0,
                        0.1,
                        False,
                        files_dir / "stdout.txt",
                        files_dir / "stderr.txt",
                    ),
                )
                return DraftMutation(submit, readback)

            def write_transaction_then_fault(
                path: Path, value: dict[str, object], phase: str
            ) -> dict[str, object]:
                result = original_write_transaction(path, value, phase)
                if faults and phase == faults[0]:
                    faults.pop(0)
                    raise KeyboardInterrupt(f"fault after {phase}")
                return result

            ledger_mark_faulted = False

            def mark_then_fault(transaction_case_dir: Path, sequence: int) -> None:
                nonlocal ledger_mark_faulted
                if not ledger_mark_faulted:
                    ledger_mark_faulted = True
                    raise KeyboardInterrupt("fault before draft journal advancement")
                original_mark(transaction_case_dir, sequence)

            with (
                patch(
                    "testinfra.tfa.pipeline.validate_witness_artifacts",
                    return_value=evidence,
                ),
                patch(
                    "testinfra.tfa.pipeline.validate_qualified_report",
                    return_value=summary,
                ),
                patch(
                    "testinfra.tfa.pipeline._qualified_report",
                    return_value=summary,
                ),
                patch(
                    "testinfra.tfa.pipeline._validation_result",
                    return_value=ValidationResult(changed, 4, 4),
                ),
                patch("testinfra.tfa.pipeline._revalidate_draft", return_value=url),
                patch("testinfra.tfa.pipeline._verify_validation_seal"),
                patch(
                    "testinfra.tfa.pipeline._current_revision",
                    side_effect=current_revision,
                ),
                patch(
                    "testinfra.tfa.pipeline.verify_local_commit",
                    return_value="committed-revision",
                ),
                patch("testinfra.tfa.pipeline.run_process", side_effect=commit_process),
                patch(
                    "testinfra.tfa.pipeline.create_draft", side_effect=draft_mutation
                ),
                patch(
                    "testinfra.tfa.pipeline._write_draft_transaction",
                    side_effect=write_transaction_then_fault,
                ),
                patch(
                    "testinfra.tfa.pipeline._comment_stage",
                    return_value={"state": "authorization-blocked"},
                ),
            ):
                for phase in ("committed", "submitted", "published"):
                    with self.assertRaisesRegex(KeyboardInterrupt, phase):
                        run_case(
                            manifest,
                            "asyncssl",
                            root / "artifacts",
                            root / "repro",
                            checkout,
                            root / "schema.json",
                            None,
                            10,
                        )
                    self.assertEqual(ledger.read()[-1].stage, Stage.VERIFIED)
                with (
                    patch(
                        "testinfra.tfa.pipeline._mark_draft_ledger_recorded",
                        side_effect=mark_then_fault,
                    ),
                    self.assertRaisesRegex(KeyboardInterrupt, "journal advancement"),
                ):
                    run_case(
                        manifest,
                        "asyncssl",
                        root / "artifacts",
                        root / "repro",
                        checkout,
                        root / "schema.json",
                        None,
                        10,
                    )
                self.assertEqual(ledger.read()[-1].stage, Stage.DIFF_DRAFTED)
                self.assertEqual(
                    json.loads((control / "draft-transaction.json").read_text())[
                        "phase"
                    ],
                    "published",
                )

                with patch(
                    "testinfra.tfa.pipeline._mark_draft_ledger_recorded",
                    side_effect=mark_then_fault,
                ):
                    run_case(
                        manifest,
                        "asyncssl",
                        root / "artifacts",
                        root / "repro",
                        checkout,
                        root / "schema.json",
                        None,
                        10,
                    )
            transaction = json.loads((control / "draft-transaction.json").read_text())
            self.assertEqual(faults, [])
            self.assertEqual(draft_calls, 2)
            self.assertEqual(external_mutations, 1)
            self.assertEqual(transaction["phase"], "ledger-recorded")
            self.assertEqual(transaction["ledger_sequence"], 6)
            self.assertEqual(ledger.read()[-1].stage, Stage.FINAL_COMMENT)

    def test_exact_base_preflight_blocks_diagnostic(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "case-checkout").mkdir()
            manifest_path = root / "cases.json"
            _manifest(manifest_path)
            with (
                patch(
                    "testinfra.tfa.pipeline.validate_witness_artifacts",
                    return_value={"pass": {}, "fail": {}},
                ),
                patch(
                    "testinfra.tfa.pipeline._comment_stage",
                    return_value={"state": "artifact-only"},
                ),
                patch(
                    "testinfra.tfa.pipeline._checkout_state",
                    side_effect=CaseCheckoutError("base differs"),
                ),
                patch("testinfra.tfa.pipeline._diagnose_candidate_locked") as diagnose,
                self.assertRaisesRegex(CaseCheckoutError, "base differs"),
            ):
                run_case(
                    load_manifest(manifest_path),
                    "asyncssl",
                    root / "artifacts",
                    root / "repro",
                    root / "case-checkout",
                    root / "schema.json",
                    None,
                    10,
                )
            diagnose.assert_not_called()
            ledger = Ledger(
                root / "artifacts" / "asyncssl" / "control" / "ledger.jsonl"
            )
            self.assertEqual(
                [entry.stage for entry in ledger.read()],
                [Stage.VALIDATED_REPRO, Stage.INITIAL_COMMENT],
            )
