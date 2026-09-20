from __future__ import annotations

import json
import copy
import hashlib
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Callable
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


HERE = Path(__file__).resolve().parent
VALIDATE = HERE.parent
sys.path.insert(0, str(VALIDATE))

import reverie_node
import reverie_safe_ci
import service_result
import start_unit
from test_immutable_tool_authority import ExecutingAuthorityFixture


DEFAULT_PASSED = object()


def count_transport_fixture(directory: Path) -> tuple[Path, dict, dict]:
    """Data-only transport input, never a claim of canonical-ledger admission."""
    path = directory / "validate-count.json"
    request = {"schema": reverie_node.COUNT_SCHEMA, "nonce": "1" * 32,
               "producer_files": dict.fromkeys(reverie_node.COUNT_SOURCE_PATHS, "2" * 64)}
    record = {
        "schema_version": 1, "producer": "ci-hub/validate/run_registry.py",
        "kind": "reverie-validate", "state": "completed", "unit": "validate-count.service",
        "repo": "rrnewton/reverie", "target": "a" * 40,
        "checkout": str(directory / "checkout"), "source_checkout": str(directory / "checkout"),
        "cargo_home": str(directory / "cargo"), "log": str(directory / "run.log"),
        "agent": "count-test", "host": "count-test", "admission": "ci-hub validate-lock", "pane_role": "observer-only",
        "temporary_checkout": False,
        "started_at": "2026-09-20T00:00:00.123456+00:00",
        "parent_checkout_head": "b" * 40, "validate_lock_child_deadline_seconds": 3600,
        "process_identity": {"pid": 123, "start_ticks": 456, "boot_id": "data-only"},
        "reverie_count_request": request, "result": "failure", "exit_code": 1,
        "final_validate_status": "FAILED", "finished_at": "2026-09-20T00:01:00+00:00",
        "canonical_verdict": "NEEDS-RERUN", "wrapper_exit_code": 4,
    }
    gates = [{"name": name, "result": "pass"} for name in reverie_node.V5_GATES]
    for gate in gates:
        if gate["name"] in reverie_node.V5_TEST_GATES:
            gate.update(executed_tests=10, passed_tests=10, filtered_tests=2)
        if gate["name"] == reverie_node.V5_TEST_GATES[0]:
            gate.update(result="fail", passed_tests=9)
    original = {
        "schema_version": 5, "producer": "reverie-validate-sh", "repo": "reverie",
        "record_id": "source-count", "commit": record["target"], "host": "count-test",
        "cwd": record["checkout"], "log_file": record["log"],
        "started_at": "2026-09-20T00:00:00+00:00", "finished_at": record["finished_at"],
        "result": "fail", "exit_code": 1, "checks": 8, "failures": 1,
        "executed_tests": 20, "passed_tests": 19, "filtered_tests": 4, "gates": gates,
    }
    row = {**copy.deepcopy(original), "schema_version": 5, "producer": "ci-hub-reverie-validate-run",
           "repo": "rrnewton/reverie", "tree": "c" * 40, "raw_result": "fail",
           "reverie_validation_policy": "reverie-local-validation/v5", "gates_expected": 8,
           "gates_run": 8, "coverage": {"planned_test_nodes": 2, "executed_test_nodes": 2,
                                        "zero_executed_nodes": [], "absent_nodes": []},
           "safe_ci": {"cgroup": "/user.slice/dagrun.slice/dagrun-321.scope/step-validate.reverie-full",
                       "required": True, "observed": True, "fallback_allowed": False,
                       "step": reverie_node.SAFE_CI_STEP},
           "validate_lock_authority": {
               "schema_version": 1, "slot": 0, "admissible": True, "lock_admissible": True,
               "state": "held", "reason_code": None, "canonical_anchor_held": True,
               "cleanup_state": "active-bound", "admission_floor": None, "admission_floor_reason": None,
               "holder": {"agent": record["agent"], "kind": record["kind"], "target": record["target"],
                          "host": record["host"], "acquired_at": 1789862400, "expires_at": 1789863600},
               "owner": {**record["process_identity"], "host": record["host"], "liveness": "alive"}}}
    source = {"schema": "validate-ledger/v1", "event_id": "source-count", "run_id": "source-count",
              "event_type": "run.result", "team": "reverie", "host": "count-test",
              "commit": record["target"], "started_at": original["started_at"],
              "emitted_at": original["finished_at"], "outcome": "fail",
              "producer": {"source": "observed", "tool": "reverie-validate-sh", "tool_version": "5"},
              "legacy_row": original}
    final = {**copy.deepcopy(source), "event_id": "final-count", "event_type": "run.correct",
             "supersedes": source["event_id"], "reason": "count fixture",
             "producer": {"source": "observed", "tool": "ci-hub-reverie-validate-run", "tool_version": "5"},
             "legacy_row": copy.deepcopy(row)}
    canonical = {"source": source, "final": final, "row": row}
    binding = {"schema": reverie_node.COUNT_BINDING_SCHEMA, "context": reverie_node.count_context(record, path),
               "invocation_id": "3" * 32, "source_id": source["event_id"], "final_id": final["event_id"],
               "run_id": source["run_id"], "canonical": canonical,
               "canonical_sha256": hashlib.sha256(reverie_node.count_json(canonical)).hexdigest()}
    step = row["safe_ci"]["cgroup"]

    def observed(unit: str, group: str, inode: int, service: bool) -> dict:
        props = {"Id": unit, "LoadState": "loaded", "ActiveState": "active", "SubState": "running",
                 "InvocationID": ("4" if service else "5") * 32, "ControlGroup": group}
        if service:
            props["Type"] = "simple"
        return {"before": copy.deepcopy(props), "after": props,
                "cgroup": {"path": group, "device": 27, "inode": inode}}

    binding["execution"] = {
        "schema": reverie_node.COUNT_OWNER_SCHEMA,
        "producer": {"pid": 789, "start_ticks": 999, "boot_id": record["process_identity"]["boot_id"]},
        "lineage": [{"pid": 789, "start_ticks": 999, "ppid": 321},
                    {"pid": 321, "start_ticks": 888, "ppid": 123},
                    {"pid": 123, "start_ticks": 456, "ppid": 100}],
        "owner_cgroup": "/user.slice/validate-count.service",
        "service": observed(record["unit"], "/user.slice/validate-count.service", 100, True),
        "scope": observed("dagrun-321.scope", step.rsplit("/", 1)[0], 101, False),
        "step": {"path": step, "device": 27, "inode": 102},
        "authority": copy.deepcopy(row["validate_lock_authority"]),
    }
    return path, record, binding


def count_evidence(binding: dict) -> dict:
    return {"schema": reverie_node.COUNT_EVIDENCE_SCHEMA, "binding": binding,
            "sidecar_sha256": hashlib.sha256(reverie_node.count_json(binding)).hexdigest()}


class ReverieCountTransportTests(unittest.TestCase):
    def test_transport_failure_preserves_each_original_main_exit(self) -> None:
        for raw, status in ((0, "PASSED"), (1, "FAILED"), (75, "COULD_NOT_RUN")):
            with self.subTest(raw=raw), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                checkout = root / "reverie"
                checkout.mkdir()
                with mock.patch.object(reverie_node, "produce_count_handoff", side_effect=OSError("sidecar refused")):
                    result, final, _row = ReverieValidateTest.run_reverie_node(root, checkout, validate_exit=raw)
                self.assertEqual(raw, result)
                self.assertEqual(f"FINAL_VALIDATE_STATUS: {status}\n", final)

    def test_failed_execution_counts_and_repeated_gate_executions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path, record, binding = count_transport_fixture(Path(temporary))
            self.assertEqual((20, 19, 4), reverie_node.validate_count_binding(binding, record, path))
            self.assertEqual("fail", binding["canonical"]["row"]["result"])
            # These are executions in two gates, never ten globally unique tests.
            self.assertEqual(2, len(reverie_node.V5_TEST_GATES))

    def test_wrong_handle_source_generation_or_invocation_refuses(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path, record, binding = count_transport_fixture(Path(temporary))
            for field in ("unit", "target", "checkout", "log", "started_at", "parent_checkout_head", "process_identity"):
                with self.subTest(field=field):
                    changed = copy.deepcopy(binding)
                    changed["context"][field] = "different"
                    with self.assertRaises(reverie_node.Refusal):
                        reverie_node.validate_count_binding(changed, record, path)
            for nonce in ("", "g" * 32):
                changed = copy.deepcopy(binding)
                changed["invocation_id"] = nonce
                with self.assertRaises(reverie_node.Refusal):
                    reverie_node.validate_count_binding(changed, record, path)

    def test_counts_refuse_boolean_overflow_partial_and_changed_policy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path, record, binding = count_transport_fixture(Path(temporary))
            for field, value in (("executed_tests", True), ("passed_tests", 21),
                                 ("filtered_tests", 1 << 63), ("executed_tests", None),
                                 ("reverie_validation_policy", "invented")):
                with self.subTest(field=field, value=value):
                    changed = copy.deepcopy(binding)
                    changed["canonical"]["row"][field] = value
                    changed["canonical_sha256"] = hashlib.sha256(reverie_node.count_json(changed["canonical"])).hexdigest()
                    with self.assertRaises(reverie_node.Refusal):
                        reverie_node.validate_count_binding(changed, record, path)

    def test_exclusive_sidecar_retry_conflict_and_unsafe_reads(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path, _record, binding = count_transport_fixture(Path(temporary))
            sidecar = reverie_node.count_sidecar_path(path)
            reverie_node.publish_count_sidecar(sidecar, binding)
            first = sidecar.read_bytes()
            reverie_node.publish_count_sidecar(sidecar, binding)
            changed = copy.deepcopy(binding)
            changed["run_id"] = "second-run"
            with self.assertRaisesRegex(reverie_node.Refusal, "different count sidecar"):
                reverie_node.publish_count_sidecar(sidecar, changed)
            self.assertEqual(first, sidecar.read_bytes())
            link = path.with_name("link.json")
            link.symlink_to(sidecar)
            with self.assertRaises(OSError):
                reverie_node.read_count_sidecar(link)
            for raw in (b'{"x":1,"x":2}\n', b'{}', b'{"x":NaN}\n'):
                malformed = path.with_name("malformed.json")
                malformed.write_bytes(raw)
                with self.assertRaises((reverie_node.Refusal, ValueError)):
                    reverie_node.read_count_sidecar(malformed)

    def test_unavailable_canonical_reader_is_not_sidecar_acceptance(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            path, record, binding = count_transport_fixture(directory)
            reverie_node.publish_count_sidecar(reverie_node.count_sidecar_path(path), binding)
            with mock.patch.object(reverie_node, "read_finalization", side_effect=reverie_node.Refusal("checker unavailable")):
                with self.assertRaisesRegex(reverie_node.Refusal, "checker unavailable"):
                    reverie_node.consume_count_handoff(directory, directory, path, record)


class ReverieCountOwnerTests(unittest.TestCase):
    def test_real_holder_owner_and_sibling_scope_with_renewed_lease(self) -> None:
        path, record, binding = count_transport_fixture(Path("/data-only"))
        proof = binding["execution"]
        self.assertNotIn(record["unit"], proof["step"]["path"].split("/"))
        self.assertEqual({"agent", "kind", "target", "host", "acquired_at", "expires_at"},
                         set(proof["authority"]["holder"]))
        old = copy.deepcopy(binding["canonical"]["row"]["validate_lock_authority"])
        proof["authority"]["holder"]["acquired_at"] += 60
        proof["authority"]["holder"]["expires_at"] += 60
        self.assertEqual((20, 19, 4), reverie_node.validate_count_binding(binding, record, path))
        self.assertEqual(old, binding["canonical"]["row"]["validate_lock_authority"])
        self.assertEqual("NEEDS-RERUN", record["canonical_verdict"])

    def test_unrelated_owner_movement_and_invented_holder_refuse(self) -> None:
        path, record, binding = count_transport_fixture(Path("/data-only"))
        changes = [
            lambda p: p["lineage"][-1].update(start_ticks=457),
            lambda p: p["lineage"][0].update(ppid=999),
            lambda p: p["producer"].update(boot_id="another-boot"),
            lambda p: p["authority"]["owner"].update(pid=124),
            lambda p: p["authority"]["holder"].update(agent="unrelated"),
            lambda p: p["authority"]["holder"].update(host="other-host"),
            lambda p: p["authority"].update(admissible=False),
            lambda p: p["authority"].update(canonical_anchor_held=False),
            lambda p: p["authority"].update(cleanup_state=[]),
            lambda p: p["authority"].update(slot=1),
            lambda p: p["authority"].update(holder={k: record[k] for k in
                                                   ("unit", "target", "started_at", "process_identity")}),
            lambda p: p["service"]["after"].update(InvocationID="6" * 32),
            lambda p: p["scope"]["after"].update(ControlGroup="/unrelated/dagrun-321.scope"),
            lambda p: p.update(owner_cgroup="/user.slice/unrelated.service"),
            lambda p: p["step"].update(path="/user.slice/dagrun.slice/dagrun-999.scope/step-validate.reverie-full"),
            lambda p: p["step"].update(inode=True),
            lambda p: p["authority"]["holder"].update(expires_at=True),
        ]
        for index, change in enumerate(changes):
            with self.subTest(index=index):
                bad = copy.deepcopy(binding)
                change(bad["execution"])
                with self.assertRaises(reverie_node.Refusal):
                    reverie_node.validate_count_binding(bad, record, path)

    def test_explicit_unit_properties_allow_live_activating_oneshot(self) -> None:
        _path, record, binding = count_transport_fixture(Path("/data-only"))
        props = binding["execution"]["service"]["before"]
        props.update(Type="oneshot", ActiveState="activating", SubState="start")
        raw = "".join(f"{key}={value}\n" for key, value in props.items())
        self.assertEqual(props, reverie_node._parse_count_unit(raw, record["unit"], service=True))
        for bad in (raw + "Id=second.service\n", raw.replace("LoadState=loaded", "LoadState=not-found"),
                    raw.replace("InvocationID=" + "4" * 32, "InvocationID="),
                    raw.replace("ControlGroup=/user.slice/validate-count.service\n", ""),
                    raw + "Unknown=1\n", raw + "\x00\n"):
            with self.subTest(raw=bad), self.assertRaises(reverie_node.Refusal):
                reverie_node._parse_count_unit(bad, record["unit"], service=True)

    def test_proc_parsing_uses_generation_and_exact_unified_membership(self) -> None:
        raw = "789 (name with ) spaces) S 321 " + "0 " * 17 + "999 0\n"
        self.assertEqual({"pid": 789, "ppid": 321, "start_ticks": 999},
                         reverie_node._parse_count_stat(raw, 789))
        for bad in (raw.replace(") S ", ") Z "), raw.replace("789 (", "788 ("), "789 (short) R 321"):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node._parse_count_stat(bad, 789)
        self.assertEqual("/a/b", reverie_node._parse_count_membership("0::/a/b\n"))
        for bad in ("1:cpu:/a/b\n", "0::/a/../b\n", "0::/a/b\n0::/c/d\n"):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node._parse_count_membership(bad)

    def test_deadline_refuses_before_spawn_and_cleanup_does_not_reset_grace(self) -> None:
        with mock.patch.object(reverie_node.time, "monotonic", return_value=9), \
                mock.patch.object(reverie_node.subprocess, "Popen") as spawn:
            with self.assertRaisesRegex(reverie_node.Refusal, "deadline"):
                reverie_node._capture_count_query(["never-run"], Path("/"), 10, stdout_limit=10)
            spawn.assert_not_called()
        proc = mock.Mock(pid=321, returncode=None)
        status = reverie_node._CountQueryStatus(proc, 52)
        status.authenticated = True
        now = [8.5]
        sleeps = []

        def advance(duration):
            sleeps.append(duration)
            now[0] += duration

        with mock.patch.object(reverie_node.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(reverie_node.time, "sleep", side_effect=advance), \
                mock.patch.object(reverie_node.os, "waitid", return_value=None) as waitid, \
                mock.patch.object(reverie_node.signal, "pidfd_send_signal") as signal_child:
            result = reverie_node._cleanup_count_query(status, 10)
        self.assertEqual(10, now[0])
        self.assertAlmostEqual(1.5, sum(sleeps))
        self.assertTrue(all(0 < duration <= 0.05 for duration in sleeps))
        waitid.assert_called_with(reverie_node.os.P_PIDFD, 52, reverie_node.os.WEXITED | reverie_node.os.WNOHANG)
        self.assertEqual([mock.call(52, reverie_node.signal.SIGTERM), mock.call(52, reverie_node.signal.SIGKILL)],
                         signal_child.call_args_list)
        self.assertIn("unknown at deadline", result)
        proc.poll.assert_not_called()
        proc.wait.assert_not_called()
        with mock.patch.object(reverie_node.signal, "pidfd_send_signal") as no_signal:
            self.assertIn("identity/cleanup unknown", reverie_node._cleanup_count_query(None, 10))
            no_signal.assert_not_called()

    def test_pidfd_must_belong_to_our_waitable_child_before_signaling(self) -> None:
        proc = mock.Mock(pid=321, returncode=None)
        proc.poll.return_value = None
        with mock.patch.object(reverie_node.subprocess, "Popen", return_value=proc), \
                mock.patch.object(reverie_node.time, "monotonic", return_value=0), \
                mock.patch.object(reverie_node.os, "pidfd_open", return_value=52), \
                mock.patch.object(reverie_node.os, "waitid", side_effect=ChildProcessError("not our child")), \
                mock.patch.object(reverie_node.os, "close") as close, \
                mock.patch.object(reverie_node.signal, "pidfd_send_signal") as no_signal:
            with self.assertRaisesRegex(reverie_node.Refusal, "identity/cleanup unknown"):
                reverie_node._capture_count_query(["not-executed"], Path("/"), 10, stdout_limit=10)
        no_signal.assert_not_called()
        close.assert_called_once_with(52)
        proc.stdout.close.assert_called_once()
        proc.stderr.close.assert_called_once()

    def test_missing_pidfd_api_refuses_before_creating_a_query(self) -> None:
        for api in ("pidfd_open", "waitid"):
            with self.subTest(api=api), \
                    mock.patch.object(reverie_node.time, "monotonic", return_value=0), \
                    mock.patch.object(reverie_node.os, api, None), \
                    mock.patch.object(reverie_node.subprocess, "Popen") as spawn:
                with self.assertRaisesRegex(reverie_node.Refusal, "PIDfd ownership APIs before spawn"):
                    reverie_node._capture_count_query(["never-run"], Path("/"), 10, stdout_limit=10)
                spawn.assert_not_called()

    def test_capture_requires_complete_bounded_utf8_and_real_zero_exit(self) -> None:
        for output, status, expected in ((b"valid\n", 0, "valid\n"), (b"oversized", 0, None),
                                          (b"\xff", 0, None), (b"valid\n", 7, None)):
            with self.subTest(output=output, status=status):
                streams = [mock.Mock(), mock.Mock()]
                for descriptor, stream in zip((10, 11), streams):
                    stream.fileno.return_value = descriptor
                proc = mock.Mock(pid=321, stdout=streams[0], stderr=streams[1], returncode=0)
                event = SimpleNamespace(si_pid=321, si_code=os.CLD_EXITED, si_status=status)
                pending = {}
                selector = mock.MagicMock()
                selector.__enter__.return_value = selector
                selector.get_map.side_effect = lambda: pending
                selector.register.side_effect = lambda stream, _events, name: pending.update(
                    {stream.fileno(): SimpleNamespace(fd=stream.fileno(), fileobj=stream, data=name)})
                selector.unregister.side_effect = lambda stream: pending.pop(stream.fileno())
                selector.select.side_effect = lambda _timeout: [(next(iter(pending.values())), 1)]
                chunks = {10: [output, b""], 11: [b""]}
                with mock.patch.object(reverie_node.subprocess, "Popen", return_value=proc), \
                        mock.patch.object(reverie_node.selectors, "DefaultSelector", return_value=selector), \
                        mock.patch.object(reverie_node.time, "monotonic", return_value=0), \
                        mock.patch.object(reverie_node.os, "pidfd_open", return_value=52), \
                        mock.patch.object(reverie_node.os, "waitid", return_value=event) as waitid, \
                        mock.patch.object(reverie_node.os, "set_blocking"), \
                        mock.patch.object(reverie_node.os, "read", side_effect=lambda fd, _size: chunks[fd].pop(0)), \
                        mock.patch.object(reverie_node.os, "close") as close, \
                        mock.patch.object(reverie_node.signal, "pidfd_send_signal") as no_signal:
                    if expected is None:
                        with self.assertRaises(reverie_node.Refusal):
                            reverie_node._capture_count_query(["not-executed"], Path("/"), 10, stdout_limit=6)
                    else:
                        self.assertEqual(expected, reverie_node._capture_count_query(
                            ["not-executed"], Path("/"), 10, stdout_limit=6))
                waitid.assert_called_once_with(os.P_PIDFD, 52, os.WEXITED | os.WNOHANG)
                close.assert_called_once_with(52)
                no_signal.assert_not_called()
                proc.poll.assert_not_called()
                proc.wait.assert_not_called()
                for stream in streams:
                    stream.close.assert_called_once()

    def test_directory_replacement_refuses_held_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            named = Path(temporary) / "owned-data"
            named.mkdir()
            descriptor = reverie_node._open_count_directory(str(named))
            try:
                reverie_node._count_directory_unchanged(str(named), descriptor)
                named.rename(Path(temporary) / "previous-data")
                named.mkdir()
                with self.assertRaisesRegex(reverie_node.Refusal, "identity moved"):
                    reverie_node._count_directory_unchanged(str(named), descriptor)
            finally:
                os.close(descriptor)

    def test_exited_query_with_inherited_pipe_is_deadline_refusal_not_success(self) -> None:
        proc = mock.Mock(pid=321, returncode=0)
        event = SimpleNamespace(si_pid=321, si_code=os.CLD_EXITED, si_status=0)
        selector = mock.MagicMock()
        selector.__enter__.return_value = selector
        selector.get_map.return_value = {10: "pipe still held"}
        selector.select.return_value = []
        with mock.patch.object(reverie_node.subprocess, "Popen", return_value=proc), \
                mock.patch.object(reverie_node.selectors, "DefaultSelector", return_value=selector), \
                mock.patch.object(reverie_node.time, "monotonic", side_effect=[0, 0, 8]), \
                mock.patch.object(reverie_node.os, "pidfd_open", return_value=52), \
                mock.patch.object(reverie_node.os, "waitid", return_value=event), \
                mock.patch.object(reverie_node.os, "set_blocking"), \
                mock.patch.object(reverie_node.os, "close"), \
                mock.patch.object(reverie_node.signal, "pidfd_send_signal") as no_signal:
            with self.assertRaisesRegex(reverie_node.Refusal, "deadline exhausted.*direct child exit=0"):
                reverie_node._capture_count_query(["not-executed"], Path("/"), 10, stdout_limit=6)
        proc.communicate.assert_not_called()
        proc.poll.assert_not_called()
        proc.wait.assert_not_called()
        no_signal.assert_not_called()
        proc.stdout.close.assert_called_once()
        proc.stderr.close.assert_called_once()

    def test_pidfd_terminal_event_is_consumed_once_independent_of_popen_bookkeeping(self) -> None:
        for code, value, expected in ((os.CLD_EXITED, 0, 0), (os.CLD_EXITED, 23, 23),
                                      (os.CLD_KILLED, reverie_node.signal.SIGTERM, -reverie_node.signal.SIGTERM),
                                      (os.CLD_DUMPED, reverie_node.signal.SIGABRT, -reverie_node.signal.SIGABRT)):
            with self.subTest(code=code, value=value):
                proc = mock.Mock(pid=321, returncode=0)
                status = reverie_node._CountQueryStatus(proc, 52)
                event = SimpleNamespace(si_pid=321, si_code=code, si_status=value)
                with mock.patch.object(reverie_node.os, "waitid", side_effect=[None, event]) as waitid:
                    self.assertIsNone(status.poll())
                    self.assertTrue(status.authenticated)
                    self.assertEqual(expected, status.poll())
                    self.assertIs(status.event, event)
                    self.assertEqual(expected, proc.returncode)
                    proc.returncode = 0  # Bookkeeping cannot replace the retained event.
                    self.assertEqual(expected, status.poll())
                    self.assertEqual(2, waitid.call_count)
                proc.poll.assert_not_called()
                proc.wait.assert_not_called()


    def test_pidfd_unavailable_status_after_authentication_is_sticky_unknown(self) -> None:
        proc = mock.Mock(pid=321, returncode=0)
        status = reverie_node._CountQueryStatus(proc, 52)
        with mock.patch.object(reverie_node.os, "waitid", side_effect=[None, ChildProcessError(10, "No child processes")]) as waitid, \
                mock.patch.object(reverie_node.signal, "pidfd_send_signal") as no_signal:
            self.assertIsNone(status.poll())
            self.assertTrue(status.authenticated)
            with self.assertRaisesRegex(reverie_node.Refusal, "wait status unavailable.*ChildProcessError"):
                status.poll()
            self.assertIsNone(status.event)
            self.assertIsNone(status.returncode)
            with self.assertRaisesRegex(reverie_node.Refusal, "wait status unavailable.*ChildProcessError"):
                status.poll()
            cleanup = reverie_node._cleanup_count_query(status, 10)
            self.assertIn("status/cleanup unknown", cleanup)
            self.assertNotIn("exit=0", cleanup)
            self.assertEqual(2, waitid.call_count)
            no_signal.assert_not_called()
        proc.poll.assert_not_called()
        proc.wait.assert_not_called()


    def test_capture_valid_output_with_unavailable_status_refuses_popen_zero(self) -> None:
        streams = [mock.Mock(), mock.Mock()]
        for descriptor, stream in zip((10, 11), streams):
            stream.fileno.return_value = descriptor
        proc = mock.Mock(pid=321, stdout=streams[0], stderr=streams[1], returncode=0)
        proc.poll.return_value = 0  # The old capture would accept this after EOF.
        pending = {}
        selector = mock.MagicMock()
        selector.__enter__.return_value = selector
        selector.get_map.side_effect = lambda: pending
        selector.register.side_effect = lambda stream, _events, name: pending.update(
            {stream.fileno(): SimpleNamespace(fd=stream.fileno(), fileobj=stream, data=name)})
        selector.unregister.side_effect = lambda stream: pending.pop(stream.fileno())
        selector.select.side_effect = lambda _timeout: [(next(iter(pending.values())), 1)]
        chunks = {10: [b"valid\n", b""], 11: [b""]}
        wait_calls = []

        def real_status_shape(*args):
            wait_calls.append(args)
            if len(wait_calls) > 1 and not pending:
                raise ChildProcessError(10, "No child processes")
            return None

        with mock.patch.object(reverie_node.subprocess, "Popen", return_value=proc), \
                mock.patch.object(reverie_node.selectors, "DefaultSelector", return_value=selector), \
                mock.patch.object(reverie_node.time, "monotonic", return_value=0), \
                mock.patch.object(reverie_node.os, "pidfd_open", return_value=52), \
                mock.patch.object(reverie_node.os, "waitid", side_effect=real_status_shape), \
                mock.patch.object(reverie_node.os, "set_blocking"), \
                mock.patch.object(reverie_node.os, "read", side_effect=lambda fd, _size: chunks[fd].pop(0)), \
                mock.patch.object(reverie_node.os, "close") as close, \
                mock.patch.object(reverie_node.signal, "pidfd_send_signal") as no_signal:
            with self.assertRaisesRegex(reverie_node.Refusal, "wait status unavailable.*status/cleanup unknown"):
                reverie_node._capture_count_query(["not-executed"], Path("/"), 10, stdout_limit=6)
        self.assertEqual({10: [], 11: []}, chunks)
        close.assert_called_once_with(52)
        no_signal.assert_not_called()
        proc.poll.assert_not_called()
        proc.wait.assert_not_called()
        for stream in streams:
            stream.close.assert_called_once()


    def test_producer_queries_canonical_first_and_holds_snapshot_through_publication(self) -> None:
        import run_registry
        path, record, binding = count_transport_fixture(Path("/data-only"))
        order = []

        @contextmanager
        def observation(*_args):
            order.append("observe")
            yield binding["execution"]
            order.append("close")

        def canonical(*_args):
            order.append("canonical")
            return binding["canonical"]

        def publish(_path, value):
            self.assertEqual(binding, value)
            order.append("publish")

        ack = json.dumps({"schema": "ci-hub-validate-ledger-record/v1", "run_id": binding["run_id"],
                          "event_id": binding["final_id"]})
        with mock.patch.object(run_registry, "read_record", return_value=record), \
                mock.patch.object(reverie_node, "count_source_hashes", return_value=record["reverie_count_request"]["producer_files"]), \
                mock.patch.object(reverie_node, "read_finalization", side_effect=canonical), \
                mock.patch.object(reverie_node, "observe_count_owner", side_effect=observation), \
                mock.patch.object(reverie_node, "publish_count_sidecar", side_effect=publish), \
                mock.patch.dict(os.environ, {reverie_node.COUNT_HANDLE_ENV: str(path), "INVOCATION_ID": "3" * 32}):
            reverie_node.produce_count_handoff(Path("/data-only"), binding["canonical"]["row"],
                                                binding["canonical"]["source"], ack)
        self.assertEqual(["canonical", "observe", "publish", "close"], order)

    def test_malformed_canonical_counts_or_authority_refuse_before_live_queries(self) -> None:
        import run_registry
        for field in ("executed_tests", "validate_lock_authority"):
            with self.subTest(field=field):
                path, record, binding = count_transport_fixture(Path("/data-only"))
                binding["canonical"]["row"][field] = None
                ack = json.dumps({"schema": "ci-hub-validate-ledger-record/v1", "run_id": binding["run_id"],
                                  "event_id": binding["final_id"]})
                with mock.patch.object(run_registry, "read_record", return_value=record), \
                        mock.patch.object(reverie_node, "count_source_hashes", return_value=record["reverie_count_request"]["producer_files"]), \
                        mock.patch.object(reverie_node, "read_finalization", return_value=binding["canonical"]), \
                        mock.patch.object(reverie_node, "observe_count_owner") as observe, \
                        mock.patch.dict(os.environ, {reverie_node.COUNT_HANDLE_ENV: str(path), "INVOCATION_ID": "3" * 32}):
                    with self.assertRaises(reverie_node.Refusal):
                        reverie_node.produce_count_handoff(Path("/data-only"), binding["canonical"]["row"],
                                                          binding["canonical"]["source"], ack)
                    observe.assert_not_called()

    def test_legacy_sealed_bytes_remain_readable_but_not_current_evidence(self) -> None:
        import run_registry
        raw = (HERE / "fixtures/reverie_count_v1.json").read_bytes()
        self.assertEqual("201bf9737f39e35abf857fe2ee7307b8e77473841fb8bf948b57a961b5046805",
                         hashlib.sha256(raw).hexdigest())
        record = json.loads(raw)
        self.assertEqual(record, run_registry.parse_current_record(record).raw)
        evidence = record["reverie_count_evidence"]
        path = Path(evidence["binding"]["context"]["record_path"])
        self.assertEqual((20, 19, 4), reverie_node.parse_stored_count_evidence(evidence, record, path))
        with self.assertRaises(reverie_node.Refusal):
            reverie_node.validate_count_evidence(evidence, record, path)
        broken = copy.deepcopy(record)
        broken["reverie_count_evidence"]["sidecar_sha256"] = "0" * 64
        with self.assertRaises(RuntimeError):
            run_registry.parse_current_record(broken)
        with tempfile.TemporaryDirectory() as temporary:
            local = Path(temporary) / "handle.json"
            local.write_bytes(raw)
            self.assertEqual(record, run_registry.read_record(local))
            with self.assertRaises(reverie_node.Refusal):
                run_registry.update_reverie_counts(local, record, evidence=evidence)
            self.assertEqual(raw, local.read_bytes())
            reverie_node.publish_count_sidecar(reverie_node.count_sidecar_path(local), evidence["binding"])
            with mock.patch.object(reverie_node, "read_finalization") as ledger:
                with self.assertRaisesRegex(reverie_node.Refusal, "legacy v1"):
                    reverie_node.consume_count_handoff(Path(temporary), Path(temporary), local, record)
                ledger.assert_not_called()

    def test_observer_retains_descriptors_and_refuses_moving_or_unrelated_identity(self) -> None:
        import run_registry
        path, record, binding = count_transport_fixture(Path("/data-only"))
        proof = binding["execution"]
        for attack in (None, "parent", "membership", "scope-generation", "unrelated", "context"):
            with self.subTest(attack=attack):
                fd_paths = {}
                closed = []
                calls = {}
                owner_bytes = "".join(f"{key}={value}\n" for key, value in
                                      {"host": record["host"], **record["process_identity"]}.items()).encode()

                def directory(name):
                    descriptor = 100 + len(fd_paths)
                    fd_paths[descriptor] = name
                    return descriptor

                def info(descriptor):
                    name = fd_paths.get(descriptor, "/owned/lock.owner")
                    inode = {proof[k]["cgroup"]["path"]: proof[k]["cgroup"]["inode"]
                             for k in ("service", "scope")}
                    inode[proof["step"]["path"]] = proof["step"]["inode"]
                    return SimpleNamespace(st_dev=27, st_ino=inode.get(name.removeprefix("/sys/fs/cgroup"), 99),
                                           st_size=len(owner_bytes), st_mtime_ns=1, st_ctime_ns=1,
                                           st_mode=reverie_node.stat.S_IFREG)

                def read_at(descriptor, name):
                    key = (fd_paths[descriptor], name)
                    calls[key] = calls.get(key, 0) + 1
                    if name == "boot_id":
                        return record["process_identity"]["boot_id"] + "\n"
                    pid = int(fd_paths[descriptor].rsplit("/", 1)[1])
                    if name == "stat":
                        entry = next(item for item in proof["lineage"] if item["pid"] == pid)
                        parent = entry["ppid"]
                        if (attack == "unrelated" and pid == 789) or (attack == "parent" and calls[key] == 2):
                            parent = 1
                        return f"{pid} (control) S {parent} " + "0 " * 17 + str(entry["start_ticks"]) + "\n"
                    if pid == 789:
                        group = proof["step"]["path"]
                        if attack == "membership" and calls[key] == 2:
                            group = "/unrelated"
                    else:
                        group = proof["owner_cgroup"]
                    return "0::" + group + "\n"

                def show(_root, unit, _deadline, *, service):
                    key = (unit, "show")
                    calls[key] = calls.get(key, 0) + 1
                    value = copy.deepcopy(proof["service" if service else "scope"]["before"])
                    if attack == "scope-generation" and not service and calls[key] == 2:
                        value["InvocationID"] = "6" * 32
                    return value

                current = copy.deepcopy(record)
                if attack == "context":
                    current["parent_checkout_head"] = "d" * 40
                with mock.patch.object(reverie_node, "_open_count_directory", side_effect=directory), \
                        mock.patch.object(reverie_node, "_read_count_at", side_effect=read_at), \
                        mock.patch.object(reverie_node, "_read_count_fd", return_value=owner_bytes), \
                        mock.patch.object(reverie_node, "_show_count_unit", side_effect=show), \
                        mock.patch.object(reverie_node, "_capture_count_query", return_value=json.dumps({"authorities": [proof["authority"]]})), \
                        mock.patch.object(reverie_node.os, "open", return_value=800), \
                        mock.patch.object(reverie_node.os, "fstat", side_effect=info), \
                        mock.patch.object(reverie_node.os, "stat", return_value=info(800)), \
                        mock.patch.object(reverie_node.os, "close", side_effect=closed.append), \
                        mock.patch.object(reverie_node.os, "getpid", return_value=789), \
                        mock.patch.object(reverie_node.time, "monotonic", return_value=0), \
                        mock.patch.object(reverie_node, "read_count_sidecar", return_value=(current, "data-only", ())), \
                        mock.patch.object(reverie_node.subprocess, "Popen", side_effect=AssertionError("data-only control")), \
                        mock.patch.dict(os.environ, {"CI_HUB_VALIDATE_LOCK_OWNER_PID": "123",
                                                     "CI_HUB_VALIDATE_LOCK_OWNER_FILE": "/owned/lock.owner"}):
                    if attack is None:
                        with reverie_node.observe_count_owner(Path("/data-only"), record, binding["canonical"]["row"], path) as actual:
                            self.assertEqual(proof, actual)
                            self.assertNotIn(101, closed)  # Held producer survives until publication.
                            self.assertNotIn(800, closed)
                    else:
                        with self.assertRaises(reverie_node.Refusal):
                            with reverie_node.observe_count_owner(Path("/data-only"), record, binding["canonical"]["row"], path):
                                self.fail("moving/unrelated owner yielded a proof")
                self.assertIn(101, closed)
                self.assertEqual(len(closed), len(set(closed)))
                self.assertTrue(set(fd_paths).issubset(closed))


class ReverieValidateTest(unittest.TestCase):
    @staticmethod
    def run_safe_ci(
        root: Path,
        checkout: Path,
        *,
        node_status: str | None,
        dag_exit: int,
    ) -> tuple[int, str, str, list[str]]:
        observed: list[str] = []

        def fake_run(command, **_kwargs):
            observed.extend(map(str, command))
            dag_path = Path(command[command.index("--dag") + 1])
            dag = json.loads(dag_path.read_text())
            node = shlex.split(dag["steps"][0]["cmd"])
            status_path = Path(node[node.index("--status-file") + 1])
            if node_status is not None:
                status_path.write_text(f"FINAL_VALIDATE_STATUS: {node_status}\n")
            return subprocess.CompletedProcess(command, dag_exit)

        stdout = StringIO()
        stderr = StringIO()
        with (
            mock.patch.object(reverie_safe_ci.subprocess, "run", side_effect=fake_run),
            mock.patch.object(reverie_safe_ci, "ROOT", root),
            mock.patch.dict(os.environ, {}, clear=True),
        ):
            with redirect_stdout(stdout), redirect_stderr(stderr):
                rc = reverie_safe_ci.main(
                    [
                        "--root",
                        str(root),
                        "--checkout",
                        str(checkout),
                        "--target",
                        "b" * 40,
                        "--log",
                        str(root / "reverie.log"),
                    ]
                )
        return rc, stdout.getvalue(), stderr.getvalue(), observed

    @staticmethod
    def run_reverie_node(
        root: Path,
        checkout: Path,
        *,
        validate_exit: int,
        legacy_v3: bool = False,
        maturity_v5: bool = False,
        regular_executed: int = 12,
        aggregate_executed: int | None = None,
        regular_passed: object = DEFAULT_PASSED,
        aggregate_passed: object = DEFAULT_PASSED,
        regular_result: str = "pass",
        mutate_source: Callable[[dict], None] | None = None,
        expect_record: bool = True,
    ) -> tuple[int, str, dict]:
        target = "b" * 40
        log = root / "reverie.log"
        status_file = root / "final-status"
        log.write_text(
            "== Test all owned public instruction cases ==\n"
            "test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n"
            "== Test regular workspace cases ==\n"
            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n"
            "== Documentation tests ==\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n"
        )

        def fake_git(_checkout: Path, *args: str) -> str:
            if args == ("rev-parse", "HEAD^{commit}"):
                return target
            if args == ("status", "--porcelain=v1"):
                return ""
            if args == ("rev-parse", "HEAD^{tree}"):
                return "c" * 40
            raise AssertionError(f"unexpected git call: {args!r}")

        expected_gates = (
            reverie_node.V3_GATES if legacy_v3 else reverie_node.EXPECTED_GATES
        )
        if maturity_v5:
            expected_gates = reverie_node.V5_GATES
        gates = []
        for name in expected_gates:
            gate = {"name": name}
            if name == "Test all owned public instruction cases":
                gate.update(
                    {
                        "result": "pass",
                        "executed_tests": 17,
                        "passed_tests": 17,
                        "filtered_tests": 0,
                    }
                )
            elif name == "Test regular workspace cases":
                gate.update(
                    {
                        "result": regular_result,
                        "executed_tests": regular_executed,
                        "passed_tests": (
                            regular_executed
                            if regular_passed is DEFAULT_PASSED
                            else regular_passed
                        ),
                        "filtered_tests": 3,
                    }
                )
            elif name == "Documentation tests":
                gate.update(
                    {
                        "result": "pass",
                        "executed_tests": 5,
                        "passed_tests": 5,
                        "filtered_tests": 1,
                    }
                )
            gates.append(gate)
        executed_tests = (
            aggregate_executed
            if aggregate_executed is not None
            else regular_executed + 5 + (0 if legacy_v3 or maturity_v5 else 17)
        )
        source = {
            "result": "pass" if validate_exit == 0 else "fail",
            "checks": len(expected_gates),
            "executed_tests": executed_tests,
            "passed_tests": (
                executed_tests
                if aggregate_passed is DEFAULT_PASSED
                else aggregate_passed
            ),
            "filtered_tests": 4,
            "gates": gates,
        }
        if mutate_source is not None:
            mutate_source(source)
        recorded: list[dict] = []
        with (
            mock.patch.object(reverie_node, "git", side_effect=fake_git),
            mock.patch.object(reverie_node, "require_kvm_environment"),
            mock.patch.object(
                reverie_node,
                "safe_ci_cgroup",
                return_value=(
                    "/user.slice/user-212630.slice/user@212630.service/dagrun.slice/"
                    "dagrun-153463.scope/step-validate.reverie-full"
                ),
            ),
            mock.patch.object(reverie_node, "lock_authority", return_value={}),
            mock.patch.object(
                reverie_node.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], validate_exit),
            ),
            mock.patch.object(reverie_node, "load_source_row", return_value=(source, {"event_id": "source-event"})),
            mock.patch.object(
                reverie_node,
                "record",
                side_effect=lambda _root, row, _source: recorded.append(row),
            ),
        ):
            rc = reverie_node.main(
                [
                    "--root",
                    str(root),
                    "--checkout",
                    str(checkout),
                    "--target",
                    target,
                    "--log",
                    str(log),
                    "--status-file",
                    str(status_file),
                ]
            )
        if expect_record:
            assert len(recorded) == 1
            row = recorded[0]
        else:
            assert recorded == []
            row = {}
        return rc, status_file.read_text(), row

    def test_systemd_child_is_safe_ci_wrapper_without_unboxed_flags(self) -> None:
        root = Path("/workspace/dev-hermit")
        checkout = Path("/workspace/reverie")
        command = start_unit.build_systemd_command(
            root=root,
            checkout=checkout,
            target="a" * 40,
            agent="codex-1",
            unit="validate-codex-1-reverie",
            record=Path("/workspace/record.json"),
            log=Path("/workspace/reverie.log"),
            pr=437,
            validate_args=[],
            wait=60,
            hold=60,
            child_deadline=8000,
            environment={"HOME": "/home/test", "PATH": "/usr/bin:/bin"},
            repo="rrnewton/reverie",
        )
        joined = " ".join(map(str, command))
        self.assertIn("ci-hub/validate/reverie_safe_ci.py", joined)
        self.assertIn("--kind reverie-validate", joined)
        self.assertIn("--repo", start_unit.parser().format_help())
        self.assertIn("REVERIE_REQUIRE_KVM=1", joined)
        self.assertNotIn("HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS", joined)
        self.assertNotIn("run_with_validate_deadline.py", joined)
        self.assertNotIn("--allow-cgroup-failure", joined)
        self.assertNotIn("--unsafe-no-cgroups", joined)
        self.assertNotIn("scripts/validate.rs", joined)

    def test_systemd_both_shapes_pass_state_separately_from_code(self) -> None:
        root = Path("/immutable/code")
        state = Path("/canonical/state")
        for prefix in (None, ["/installed/operational-tool", "--run-current-tool"]):
            with self.subTest(prefix=prefix):
                command = start_unit.build_systemd_command(
                    root=root, state_root=state, checkout=Path("/product/checkout"),
                    target="a" * 40, agent="agent", unit="validate-test",
                    record=state / "record.json", log=state / "validate.log", pr=597,
                    validate_args=[], wait=60, hold=60, child_deadline=8000,
                    environment={"HOME": "/home/test", "PATH": "/usr/bin:/bin"},
                    repo="rrnewton/reverie", unit_tool_prefix=prefix,
                )
                self.assertIn(f"DEV_HERMIT_PARENT={state}", command)
                self.assertEqual(str(state), command[command.index("--state-root") + 1])
                if prefix is None:
                    self.assertEqual(str(root), command[command.index("--root") + 1])
                    self.assertIn(str(root / "ci-hub/validate/reverie_safe_ci.py"), command)
                else:
                    self.assertIn(
                        'exec "$0" "$DEV_HERMIT_TOOL_ROOT/ci-hub/validate/reverie_safe_ci.py" '
                        '--root "$DEV_HERMIT_TOOL_ROOT" "$@"', command,
                    )
                    self.assertNotIn(str(root / "ci-hub/validate/reverie_safe_ci.py"), command)
                self.assertNotIn("--allow-cgroup-failure", command)
                self.assertNotIn("--unsafe-no-cgroups", command)

    def test_safe_ci_separates_authenticated_code_state_and_checkout(self) -> None:
        for installed in (False, True):
            with self.subTest(installed=installed), ExecutingAuthorityFixture() as fixture:
                checkout = fixture.base / "checkout"
                checkout.mkdir()
                root = fixture.tool_root if installed else fixture.code
                environment = fixture.environment() if installed else {}
                observed = []

                def fake_run(command, **kwargs):
                    observed.append(command)
                    self.assertEqual(checkout, kwargs["cwd"])
                    self.assertEqual(str(fixture.state_root), kwargs["env"]["DEV_HERMIT_PARENT"])
                    self.assertEqual(str(root / "ci-hub/bin/agent-tool"), command[0])
                    self.assertEqual("1", command[command.index("--max-steps") + 1])
                    self.assertEqual("8", command[command.index("--max-cpus") + 1])
                    self.assertEqual("7500", command[command.index("--run-timeout") + 1])
                    self.assertEqual(
                        str(fixture.state_root / "ignored/validate/reverie-safe-ci-profiles"),
                        command[command.index("--perf-dir") + 1],
                    )
                    dag_path = Path(command[command.index("--dag") + 1])
                    self.assertEqual(fixture.state_root / "ignored/validate", dag_path.parent)
                    dag = json.loads(dag_path.read_text())
                    self.assertEqual(7200, dag["default_step_timeout"])
                    self.assertEqual(1, len(dag["steps"]))
                    step = dag["steps"][0]
                    self.assertEqual((7200, 14400), (step["timeout"], step["cpu_timeout"]))
                    self.assertEqual(8, step["hint"]["preferred_inner_jobs"])
                    self.assertEqual(34359738368, step["hint"]["hard_mem_max_bytes"])
                    node = shlex.split(step["cmd"])
                    self.assertEqual(str(root / "ci-hub/validate/reverie_node.py"), node[1])
                    self.assertEqual(str(root), node[node.index("--root") + 1])
                    status = Path(node[node.index("--status-file") + 1])
                    self.assertEqual(dag_path.parent, status.parent)
                    self.assertNotIn("--allow-cgroup-failure", command)
                    self.assertNotIn("--unsafe-no-cgroups", command)
                    status.write_text("FINAL_VALIDATE_STATUS: PASSED\n")
                    return subprocess.CompletedProcess(command, 0)

                output = StringIO()
                with (
                    mock.patch.object(reverie_safe_ci, "ROOT", fixture.code),
                    mock.patch.dict(os.environ, environment, clear=True),
                    mock.patch.object(reverie_safe_ci.subprocess, "run", side_effect=fake_run),
                    redirect_stdout(output),
                ):
                    result = reverie_safe_ci.main([
                        "--root", str(root), "--state-root", str(fixture.state_root),
                        "--checkout", str(checkout), "--target", "b" * 40,
                        "--log", str(fixture.state_root / "log"),
                    ])
                self.assertEqual(0, result)
                self.assertEqual("FINAL_VALIDATE_STATUS: PASSED\n", output.getvalue())
                self.assertEqual(1, len(observed))
                self.assertFalse((fixture.code / "ignored").exists())
                self.assertEqual([], list((fixture.state_root / "ignored/validate").iterdir()))

    def test_safe_ci_refuses_wrong_or_partial_authority_before_artifacts(self) -> None:
        cases = (
            "foreign-executing-root", "foreign-argument-root", "missing-authority", "unsealed",
            "missing-tool-root", "empty-authority", "malformed-tool-root", "replaced-state",
            "state-disagreement", "missing-state-argument", "ordinary-state-disagreement",
            "ordinary-foreign-argument-root",
        )
        for case in cases:
            with self.subTest(case=case), ExecutingAuthorityFixture(sealed=case != "unsealed") as fixture:
                environment = fixture.environment()
                root = fixture.tool_root
                executing = fixture.code
                state = fixture.state_root
                if case == "foreign-executing-root":
                    executing = fixture.state_root
                elif case == "foreign-argument-root":
                    root = fixture.state_root
                elif case == "missing-authority":
                    environment.pop("DEV_HERMIT_TOOL_AUTHORITY")
                elif case == "missing-tool-root":
                    environment.pop("DEV_HERMIT_TOOL_ROOT")
                elif case == "empty-authority":
                    environment["DEV_HERMIT_TOOL_AUTHORITY"] = ""
                elif case == "malformed-tool-root":
                    environment["DEV_HERMIT_TOOL_ROOT"] = "/proc/self/fd/nope"
                elif case == "replaced-state":
                    state.rename(fixture.base / "old-state")
                    state.mkdir()
                elif case == "state-disagreement":
                    state = fixture.base
                elif case == "ordinary-state-disagreement":
                    root = fixture.code
                    environment = {"DEV_HERMIT_PARENT": str(fixture.base)}
                elif case == "ordinary-foreign-argument-root":
                    root = fixture.state_root
                    environment = {}
                arguments = [
                    "--root", str(root), "--checkout", str(fixture.base),
                    "--target", "b" * 40, "--log", str(state / "log"),
                ]
                if case != "missing-state-argument":
                    arguments += ["--state-root", str(state)]
                output, errors = StringIO(), StringIO()
                with (
                    mock.patch.object(reverie_safe_ci, "ROOT", executing),
                    mock.patch.dict(os.environ, environment, clear=True),
                    mock.patch.object(reverie_safe_ci.subprocess, "run") as run,
                    mock.patch.object(reverie_safe_ci.tempfile, "mkstemp") as temporary,
                    mock.patch.object(Path, "mkdir") as mkdir,
                    redirect_stdout(output), redirect_stderr(errors),
                ):
                    result = reverie_safe_ci.main(arguments)
                self.assertEqual(75, result)
                self.assertEqual("FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n", output.getvalue())
                self.assertIn("could not prepare validation", errors.getvalue())
                run.assert_not_called()
                temporary.assert_not_called()
                mkdir.assert_not_called()

    def test_safe_ci_state_symlink_loops_are_complete_no_result(self) -> None:
        for inherited in (False, True):
            with self.subTest(inherited=inherited), ExecutingAuthorityFixture() as fixture:
                loop = fixture.base / "loop"
                loop.symlink_to(loop.name)
                state = fixture.state_root if inherited else loop
                environment = {"DEV_HERMIT_PARENT": str(loop)} if inherited else {}
                output, errors = StringIO(), StringIO()
                with (
                    mock.patch.object(reverie_safe_ci, "ROOT", fixture.code),
                    mock.patch.dict(os.environ, environment, clear=True),
                    mock.patch.object(reverie_safe_ci.subprocess, "run") as run,
                    mock.patch.object(Path, "mkdir") as mkdir,
                    redirect_stdout(output), redirect_stderr(errors),
                ):
                    result = reverie_safe_ci.main([
                        "--root", str(fixture.code), "--state-root", str(state),
                        "--checkout", str(fixture.base), "--target", "b" * 40,
                        "--log", str(fixture.base / "log"),
                    ])
                self.assertEqual(75, result)
                self.assertEqual("FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n", output.getvalue())
                self.assertIn("cannot resolve authority path", errors.getvalue())
                run.assert_not_called()
                mkdir.assert_not_called()

    def test_safe_ci_unwritable_state_is_no_result_without_dagrun(self) -> None:
        with ExecutingAuthorityFixture() as fixture:
            fixture.state_root.chmod(0o555)
            output, errors = StringIO(), StringIO()
            with (
                mock.patch.object(reverie_safe_ci, "ROOT", fixture.code),
                mock.patch.dict(os.environ, fixture.environment(), clear=True),
                mock.patch.object(reverie_safe_ci.subprocess, "run") as run,
                redirect_stdout(output), redirect_stderr(errors),
            ):
                result = reverie_safe_ci.main([
                    "--root", str(fixture.tool_root), "--state-root", str(fixture.state_root),
                    "--checkout", str(fixture.base), "--target", "b" * 40,
                    "--log", str(fixture.state_root / "log"),
                ])
            self.assertEqual(75, result)
            self.assertEqual("FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n", output.getvalue())
            self.assertIn("Permission denied", errors.getvalue())
            self.assertFalse((fixture.state_root / "ignored").exists())
            run.assert_not_called()

    def test_safe_ci_launcher_has_no_unboxed_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, observed = self.run_safe_ci(
                root, checkout, node_status="PASSED", dag_exit=0
            )
            self.assertEqual(rc, 0)
            self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: PASSED\n")
            self.assertIn("dagrun", observed)
            self.assertNotIn("safe-ci-dag-runner", observed)
            self.assertEqual(observed[observed.index("--max-steps") + 1], "1")
            self.assertEqual(observed[observed.index("--max-cpus") + 1], "8")
            self.assertNotIn("--jobs", observed)
            self.assertNotIn("--allow-cgroup-failure", observed)
            self.assertNotIn("--unsafe-no-cgroups", observed)
            self.assertEqual(list((root / "ignored/validate").glob("reverie-dag-*")), [])
            self.assertEqual(
                list((root / "ignored/validate").glob("reverie-final-status-*")), []
            )

    def test_safe_ci_reports_product_failure_from_the_main_path(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="FAILED", dag_exit=1
            )
        self.assertEqual(rc, 1)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: FAILED\n")

    def test_safe_ci_pass_is_accepted_by_the_central_result_reader(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="PASSED", dag_exit=0
            )
        evidence = service_result.read_output_evidence(
            stdout
            + "# ci-hub/validate-lock tool COST ACTUAL wall=1.0s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=0\n"
        )
        result = service_result.classify(evidence)
        self.assertEqual(rc, 0)
        self.assertEqual(result["state"], "completed")
        self.assertEqual(result["result"], "success")

    def test_safe_ci_reports_could_not_run_from_the_main_path(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="COULD_NOT_RUN", dag_exit=1
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")

    def test_safe_ci_missing_node_status_cannot_be_reported_as_passed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, stderr, _observed = self.run_safe_ci(
                root, checkout, node_status=None, dag_exit=0
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("final validate status is absent", stderr)
        evidence = service_result.read_output_evidence(
            stdout
            + "# ci-hub/validate-lock tool COST ACTUAL wall=1.0s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=75\n"
        )
        result = service_result.classify(evidence)
        self.assertEqual(result["state"], "completed")
        self.assertEqual(result["result"], "no-result")

    def test_safe_ci_status_and_dagrun_exit_must_agree(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="PASSED", dag_exit=1
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("status disagrees with dagrun exit", stderr)

    def test_safe_ci_malformed_node_status_cannot_be_reported_as_passed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="MAYBE", dag_exit=0
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("final validate status is malformed", stderr)

    def test_safe_ci_setup_failure_still_reports_could_not_run(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            stdout = StringIO()
            stderr = StringIO()
            with (
                mock.patch.object(
                    reverie_safe_ci.tempfile,
                    "mkstemp",
                    side_effect=OSError("fixture setup failure"),
                ),
                mock.patch.object(reverie_safe_ci, "ROOT", root),
                mock.patch.dict(os.environ, {}, clear=True),
            ):
                with redirect_stdout(stdout), redirect_stderr(stderr):
                    rc = reverie_safe_ci.main(
                        [
                            "--root",
                            str(root),
                            "--checkout",
                            str(checkout),
                            "--target",
                            "b" * 40,
                            "--log",
                            str(root / "reverie.log"),
                        ]
                    )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout.getvalue(), "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("could not prepare validation", stderr.getvalue())

    def test_reverie_node_publishes_status_from_the_main_path(self) -> None:
        cases = (
            (0, 0, "FINAL_VALIDATE_STATUS: PASSED\n"),
            (1, 1, "FINAL_VALIDATE_STATUS: FAILED\n"),
            (75, 75, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"),
        )
        for validate_exit, expected_rc, expected_status in cases:
            with self.subTest(validate_exit=validate_exit):
                with tempfile.TemporaryDirectory() as raw:
                    root = Path(raw)
                    checkout = root / "reverie"
                    checkout.mkdir()
                    rc, status, row = self.run_reverie_node(
                        root, checkout, validate_exit=validate_exit
                    )
                self.assertEqual(rc, expected_rc)
                self.assertEqual(status, expected_status)
                self.assertEqual(row["executed_tests"], 34)
                self.assertEqual(row["passed_tests"], 34)
                self.assertEqual(row["filtered_tests"], 4)
                self.assertEqual(
                    row["reverie_validation_policy"], "reverie-local-validation/v4"
                )
                self.assertEqual(
                    row["coverage"],
                    {
                        "planned_test_nodes": 3,
                        "executed_test_nodes": 3,
                        "zero_executed_nodes": [],
                        "absent_nodes": [],
                    },
                )
                self.assertEqual(row["safe_ci"]["step"], reverie_node.SAFE_CI_STEP)

    def test_reverie_node_preserves_the_v3_source_contract(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, status, row = self.run_reverie_node(
                root, checkout, validate_exit=0, legacy_v3=True
            )
        self.assertEqual(rc, 0)
        self.assertEqual(status, "FINAL_VALIDATE_STATUS: PASSED\n")
        self.assertEqual(
            row["reverie_validation_policy"], "reverie-local-validation/v3"
        )
        self.assertEqual(row["gates_expected"], 7)
        self.assertEqual(row["executed_tests"], 17)
        self.assertEqual(row["passed_tests"], 17)
        self.assertEqual(row["coverage"]["planned_test_nodes"], 2)
        self.assertEqual(row["coverage"]["executed_test_nodes"], 2)

    def test_reverie_node_refuses_v4_count_and_coverage_gaps(self) -> None:
        for label, options, detail in (
            (
                "aggregate mismatch",
                {"aggregate_executed": 33},
                "aggregate test counts disagree",
            ),
            (
                "two of three test gates",
                {"regular_executed": 0},
                "requires all three counted test gates",
            ),
            (
                "passing aggregate count disagrees",
                {"aggregate_passed": 33},
                "requires passed_tests == executed_tests",
            ),
        ):
            with self.subTest(label=label), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                stderr = StringIO()
                with redirect_stderr(stderr):
                    rc, status, _row = self.run_reverie_node(
                        root,
                        checkout,
                        validate_exit=0,
                        expect_record=False,
                        **options,
                    )
                self.assertEqual(rc, 75)
                self.assertEqual(status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
                self.assertIn(detail, stderr.getvalue())

    def test_reverie_node_records_v5_without_inventing_maturity_test_counts(self) -> None:
        for validate_exit, expected in ((0, "PASSED"), (1, "FAILED"), (75, "COULD_NOT_RUN")):
            with self.subTest(validate_exit=validate_exit), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                rc, status, row = self.run_reverie_node(
                    root, checkout, validate_exit=validate_exit, maturity_v5=True
                )
            self.assertEqual(rc, validate_exit)
            self.assertEqual(status, f"FINAL_VALIDATE_STATUS: {expected}\n")
            self.assertEqual(row["reverie_validation_policy"], "reverie-local-validation/v5")
            self.assertEqual(row["gates_expected"], 8)
            self.assertEqual(row["executed_tests"], 17)
            self.assertEqual(row["passed_tests"], 17)
            self.assertEqual(row["filtered_tests"], 4)
            self.assertEqual(row["coverage"]["planned_test_nodes"], 2)
            self.assertEqual(row["coverage"]["executed_test_nodes"], 2)
            self.assertEqual(row["gates"][3], {"name": "Backend maturity"})

    def test_reverie_node_refuses_v5_count_and_coverage_gaps(self) -> None:
        for options, detail in (
            ({"aggregate_executed": 18}, "aggregate test counts disagree"),
            ({"regular_executed": 0}, "requires both counted test gates"),
            (
                {"aggregate_passed": 16},
                "requires passed_tests == executed_tests",
            ),
        ):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                stderr = StringIO()
                with redirect_stderr(stderr):
                    rc, status, _ = self.run_reverie_node(
                        root, checkout, validate_exit=0, maturity_v5=True,
                        expect_record=False, **options,
                    )
                self.assertEqual(rc, 75)
                self.assertEqual(status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
                self.assertIn(detail, stderr.getvalue())

    def test_reverie_node_refuses_missing_or_malformed_passed_counts(self) -> None:
        def remove_aggregate(source: dict) -> None:
            del source["passed_tests"]

        def remove_regular_gate(source: dict) -> None:
            regular = next(
                gate
                for gate in source["gates"]
                if gate["name"] == "Test regular workspace cases"
            )
            del regular["passed_tests"]

        for maturity_v5 in (False, True):
            for label, options, detail in (
                (
                    "missing aggregate",
                    {"mutate_source": remove_aggregate},
                    "source row passed_tests",
                ),
                (
                    "null aggregate",
                    {"aggregate_passed": None},
                    "source row passed_tests",
                ),
                (
                    "malformed aggregate",
                    {"aggregate_passed": "17"},
                    "source row passed_tests",
                ),
                (
                    "missing per-gate",
                    {"mutate_source": remove_regular_gate},
                    "Test regular workspace cases.*passed_tests",
                ),
            ):
                with (
                    self.subTest(maturity_v5=maturity_v5, label=label),
                    tempfile.TemporaryDirectory() as raw,
                ):
                    root = Path(raw)
                    checkout = root / "reverie"
                    checkout.mkdir()
                    stderr = StringIO()
                    with redirect_stderr(stderr):
                        rc, status, _ = self.run_reverie_node(
                            root,
                            checkout,
                            validate_exit=0,
                            maturity_v5=maturity_v5,
                            expect_record=False,
                            **options,
                        )
                    self.assertEqual(rc, 75)
                    self.assertEqual(
                        status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
                    )
                    self.assertRegex(stderr.getvalue(), detail)

    def test_reverie_node_compares_aggregate_and_per_gate_passed_counts(self) -> None:
        for maturity_v5, passed in ((False, 33), (True, 16)):
            with (
                self.subTest(maturity_v5=maturity_v5),
                tempfile.TemporaryDirectory() as raw,
            ):
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                stderr = StringIO()
                with redirect_stderr(stderr):
                    rc, status, _ = self.run_reverie_node(
                        root,
                        checkout,
                        validate_exit=1,
                        maturity_v5=maturity_v5,
                        aggregate_passed=passed,
                        expect_record=False,
                    )
                self.assertEqual(rc, 75)
                self.assertEqual(status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
                self.assertIn("aggregate test counts disagree", stderr.getvalue())

    def test_reverie_node_records_failed_test_counts_without_invention(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, status, row = self.run_reverie_node(
                root,
                checkout,
                validate_exit=1,
                regular_passed=11,
                aggregate_passed=33,
                regular_result="fail",
            )
        self.assertEqual(rc, 1)
        self.assertEqual(status, "FINAL_VALIDATE_STATUS: FAILED\n")
        self.assertEqual(row["result"], "fail")
        self.assertEqual(row["executed_tests"], 34)
        self.assertEqual(row["passed_tests"], 33)
        regular = next(
            gate
            for gate in row["gates"]
            if gate["name"] == "Test regular workspace cases"
        )
        self.assertEqual(regular["executed_tests"], 12)
        self.assertEqual(regular["passed_tests"], 11)
        self.assertEqual(regular["result"], "fail")

    def test_reverie_node_records_failed_owned_gate_counts_without_invention(self) -> None:
        owned = {
            "name": "Test all owned public instruction cases",
            "result": "fail",
            "exit_code": 101,
            "executed_tests": 17,
            "passed_tests": 16,
            "filtered_tests": 0,
        }

        def fail_owned_gate(source: dict) -> None:
            gate = next(gate for gate in source["gates"] if gate["name"] == owned["name"])
            gate.update(owned)

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, status, row = self.run_reverie_node(
                root,
                checkout,
                validate_exit=1,
                aggregate_passed=33,
                mutate_source=fail_owned_gate,
            )
        self.assertEqual(rc, 1)
        self.assertEqual(status, "FINAL_VALIDATE_STATUS: FAILED\n")
        self.assertEqual(row["result"], "fail")
        self.assertEqual(row["raw_result"], "fail")
        self.assertEqual(
            (row["executed_tests"], row["passed_tests"], row["filtered_tests"]),
            (34, 33, 4),
        )
        self.assertEqual(
            next(gate for gate in row["gates"] if gate["name"] == owned["name"]),
            owned,
        )

    def test_failed_owned_gate_still_requires_the_exact_population(self) -> None:
        for executed, filtered in ((16, 0), (18, 0), (17, 1)):
            with self.subTest(executed=executed, filtered=filtered):
                source = {
                    "gates": [{
                        "name": "Test all owned public instruction cases",
                        "result": "fail",
                        "exit_code": 101,
                        "executed_tests": executed,
                        "passed_tests": executed - 1,
                        "filtered_tests": filtered,
                    }],
                }
                with self.assertRaisesRegex(
                    reverie_node.Refusal,
                    "must record exactly executed_tests=17 and filtered_tests=0",
                ):
                    reverie_node.recorded_gate_coverage(
                        source, (reverie_node.V4_OWNED_PUBLIC_GATE,)
                    )

    def test_reverie_node_refusal_publishes_could_not_run(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            status_file = root / "final-status"
            with mock.patch.object(
                reverie_node, "git", side_effect=reverie_node.Refusal("not admitted")
            ):
                rc = reverie_node.main(
                    [
                        "--root",
                        str(root),
                        "--checkout",
                        str(checkout),
                        "--target",
                        "b" * 40,
                        "--log",
                        str(root / "reverie.log"),
                        "--status-file",
                        str(status_file),
                    ]
                )
            self.assertEqual(rc, 75)
            self.assertEqual(
                status_file.read_text(), "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
            )

    def test_both_test_gates_must_record_nonzero_tests(self) -> None:
        complete = {
            "gates": [
                {
                    "name": "Test regular workspace cases",
                    "result": "pass",
                    "executed_tests": 10,
                    "passed_tests": 10,
                    "filtered_tests": 3,
                },
                {
                    "name": "Documentation tests",
                    "result": "pass",
                    "executed_tests": 4,
                    "passed_tests": 4,
                    "filtered_tests": 0,
                },
            ]
        }
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                complete, reverie_node.V3_TEST_GATES
            ),
            (14, 14, 3, [], []),
        )
        complete["gates"][1]["executed_tests"] = 0
        complete["gates"][1]["passed_tests"] = 0
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                complete, reverie_node.V3_TEST_GATES
            ),
            (10, 10, 3, ["Documentation tests"], []),
        )
        missing = {"gates": complete["gates"][:1]}
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                missing, reverie_node.V3_TEST_GATES
            ),
            (10, 10, 3, [], ["Documentation tests"]),
        )

    def test_missing_per_gate_count_refuses_by_field_name(self) -> None:
        source = {
            "gates": [
                {
                    "name": "Test regular workspace cases",
                    "result": "pass",
                    "executed_tests": 10,
                    "passed_tests": 10,
                    "filtered_tests": 3,
                },
                {
                    "name": "Documentation tests",
                    "result": "pass",
                    "passed_tests": 0,
                    "filtered_tests": 0,
                },
            ]
        }
        with self.assertRaisesRegex(
            reverie_node.Refusal, "Documentation tests.*executed_tests"
        ):
            reverie_node.recorded_gate_coverage(
                source, reverie_node.V3_TEST_GATES
            )

    def test_per_gate_passed_counts_are_typed_and_consistent(self) -> None:
        def source(*, result: str = "pass", passed: object = 10) -> dict:
            return {
                "gates": [
                    {
                        "name": "Test regular workspace cases",
                        "result": result,
                        "executed_tests": 10,
                        "passed_tests": passed,
                        "filtered_tests": 3,
                    },
                    {
                        "name": "Documentation tests",
                        "result": "pass",
                        "executed_tests": 4,
                        "passed_tests": 4,
                        "filtered_tests": 0,
                    },
                ]
            }

        for value in (None, "10", True, -1):
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    reverie_node.Refusal,
                    "Test regular workspace cases.*passed_tests",
                ):
                    reverie_node.recorded_gate_coverage(
                        source(passed=value), reverie_node.V3_TEST_GATES
                    )
        missing = source()
        del missing["gates"][0]["passed_tests"]
        with self.assertRaisesRegex(
            reverie_node.Refusal, "Test regular workspace cases.*passed_tests"
        ):
            reverie_node.recorded_gate_coverage(
                missing, reverie_node.V3_TEST_GATES
            )
        with self.assertRaisesRegex(reverie_node.Refusal, "must not exceed"):
            reverie_node.recorded_gate_coverage(
                source(result="fail", passed=11), reverie_node.V3_TEST_GATES
            )
        with self.assertRaisesRegex(
            reverie_node.Refusal, "requires passed_tests == executed_tests"
        ):
            reverie_node.recorded_gate_coverage(
                source(passed=9), reverie_node.V3_TEST_GATES
            )
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                source(result="fail", passed=9), reverie_node.V3_TEST_GATES
            ),
            (14, 13, 3, [], []),
        )

    def test_per_gate_counts_fit_i64_and_aggregate_with_checked_add(self) -> None:
        maximum = reverie_node.I64_MAX

        def one_gate(*, result: str = "pass") -> dict:
            return {
                "gates": [
                    {
                        "name": "counted",
                        "result": result,
                        "executed_tests": maximum,
                        "passed_tests": maximum if result == "pass" else maximum - 1,
                        "filtered_tests": maximum,
                    }
                ]
            }

        self.assertEqual(
            reverie_node.recorded_gate_coverage(one_gate(), ("counted",)),
            (maximum, maximum, maximum, [], []),
        )
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                one_gate(result="fail"), ("counted",)
            ),
            (maximum, maximum - 1, maximum, [], []),
            "a failed row retains its exact passed/executed triplet",
        )

        for field in reverie_node.TEST_COUNT_FIELDS:
            for label, value in (
                ("null", None),
                ("string", "17"),
                ("float", 17.0),
                ("boolean", True),
                ("negative", -1),
                ("above-i64", maximum + 1),
            ):
                with self.subTest(field=field, shape=label):
                    source = one_gate()
                    source["gates"][0][field] = value
                    with self.assertRaisesRegex(
                        reverie_node.Refusal,
                        rf"{field}.*nonnegative signed 64-bit integer",
                    ):
                        reverie_node.recorded_gate_coverage(source, ("counted",))

        overflowing = {
            "gates": [
                {
                    "name": "first",
                    "result": "pass",
                    "executed_tests": maximum,
                    "passed_tests": maximum,
                    "filtered_tests": maximum,
                },
                {
                    "name": "second",
                    "result": "pass",
                    "executed_tests": 1,
                    "passed_tests": 1,
                    "filtered_tests": 1,
                },
            ]
        }
        with self.assertRaisesRegex(
            reverie_node.Refusal,
            "aggregate exceeds signed 64-bit maximum for "
            "executed_tests, passed_tests, filtered_tests",
        ):
            reverie_node.recorded_gate_coverage(
                overflowing, ("first", "second")
            )

    def test_v4_owned_public_gate_requires_exact_counts(self) -> None:
        def source(executed: int, passed: int, filtered: int) -> dict:
            return {
                "gates": [
                    {
                        "name": "Test all owned public instruction cases",
                        "result": "pass",
                        "executed_tests": executed,
                        "passed_tests": passed,
                        "filtered_tests": filtered,
                    },
                    {
                        "name": "Test regular workspace cases",
                        "result": "pass",
                        "executed_tests": 12,
                        "passed_tests": 12,
                        "filtered_tests": 3,
                    },
                    {
                        "name": "Documentation tests",
                        "result": "pass",
                        "executed_tests": 5,
                        "passed_tests": 5,
                        "filtered_tests": 1,
                    },
                ]
            }

        self.assertEqual(
            reverie_node.recorded_gate_coverage(source(17, 17, 0)),
            (34, 34, 4, [], []),
        )
        for executed, filtered in ((16, 0), (18, 0), (17, 1), (0, 0)):
            with self.subTest(executed=executed, filtered=filtered):
                with self.assertRaisesRegex(
                    reverie_node.Refusal, "must record exactly executed_tests=17"
                ):
                    reverie_node.recorded_gate_coverage(
                        source(executed, executed, filtered)
                    )
        with self.assertRaisesRegex(
            reverie_node.Refusal, "requires passed_tests == executed_tests"
        ):
            reverie_node.recorded_gate_coverage(source(17, 16, 0))

    def test_aggregate_counts_come_from_the_framework_row(self) -> None:
        self.assertEqual(
            reverie_node.recorded_test_counts(
                {
                    "result": "pass",
                    "executed_tests": 17,
                    "passed_tests": 17,
                    "filtered_tests": 4,
                }
            ),
            (17, 17, 4),
        )
        maximum = reverie_node.I64_MAX
        self.assertEqual(
            reverie_node.recorded_test_counts(
                {
                    "result": "pass",
                    "executed_tests": maximum,
                    "passed_tests": maximum,
                    "filtered_tests": maximum,
                }
            ),
            (maximum, maximum, maximum),
        )
        for field in reverie_node.TEST_COUNT_FIELDS:
            for label, value in (
                ("null", None),
                ("string", "17"),
                ("float", 17.0),
                ("boolean", True),
                ("negative", -1),
                ("above-i64", maximum + 1),
            ):
                with self.subTest(field=field, shape=label):
                    row = {
                        "result": "pass",
                        "executed_tests": maximum,
                        "passed_tests": maximum,
                        "filtered_tests": maximum,
                        field: value,
                    }
                    with self.assertRaisesRegex(
                        reverie_node.Refusal,
                        rf"{field}.*nonnegative signed 64-bit integer",
                    ):
                        reverie_node.recorded_test_counts(row)

        missing = {
            "result": "pass",
            "executed_tests": 17,
            "filtered_tests": 4,
        }
        with self.assertRaisesRegex(reverie_node.Refusal, "passed_tests"):
            reverie_node.recorded_test_counts(missing)
        with self.assertRaisesRegex(reverie_node.Refusal, "must not exceed"):
            reverie_node.recorded_test_counts(
                {
                    "result": "fail",
                    "executed_tests": 17,
                    "passed_tests": 18,
                    "filtered_tests": 4,
                }
            )
        with self.assertRaisesRegex(
            reverie_node.Refusal, "requires passed_tests == executed_tests"
        ):
            reverie_node.recorded_test_counts(
                {
                    "result": "pass",
                    "executed_tests": 17,
                    "passed_tests": 16,
                    "filtered_tests": 4,
                }
            )
        self.assertEqual(
            reverie_node.recorded_test_counts(
                {
                    "result": "fail",
                    "executed_tests": 17,
                    "passed_tests": 16,
                    "filtered_tests": 4,
                }
            ),
            (17, 16, 4),
        )

    def test_versioned_gate_contracts_are_exact(self) -> None:
        self.assertEqual(
            reverie_node.EXPECTED_GATES,
            (
                "Cross-client skill discovery",
                "Build workspace",
                "DBT virtual identity and pidfd_open policy",
                "Compile owned public instruction target",
                "Inventory owned public instruction target",
                "Test all owned public instruction cases",
                "Test regular workspace cases",
                "Documentation tests",
                "Clippy",
                "Rustfmt",
            ),
        )
        current = [{"name": name} for name in reverie_node.EXPECTED_GATES]
        policy, gates, test_gates = reverie_node.require_expected_gate_set(current)
        self.assertEqual(policy, "reverie-local-validation/v4")
        self.assertEqual(gates, reverie_node.V4_GATES)
        self.assertEqual(test_gates, reverie_node.V4_TEST_GATES)
        old = [{"name": name} for name in reverie_node.V3_GATES]
        self.assertEqual(
            reverie_node.require_expected_gate_set(old),
            (
                "reverie-local-validation/v3",
                reverie_node.V3_GATES,
                reverie_node.V3_TEST_GATES,
            ),
        )
        invalid_sets = {
            "retired merge gate": current[:1]
            + [{"name": "Merge-gate policy"}]
            + current[1:],
            "missing gate": current[:-1],
            "reordered gates": [current[1], current[0], *current[2:]],
            "v3 plus one v4 gate": old[:3] + current[3:4] + old[3:],
        }
        for index in range(3, 6):
            invalid_sets[f"missing added gate {index}"] = (
                current[:index] + current[index + 1 :]
            )
            reordered = list(current)
            reordered[index - 1], reordered[index] = (
                reordered[index],
                reordered[index - 1],
            )
            invalid_sets[f"reordered added gate {index}"] = reordered
        for case, gates in invalid_sets.items():
            with self.subTest(case=case):
                with self.assertRaisesRegex(reverie_node.Refusal, "gate set drifted"):
                    reverie_node.require_expected_gate_set(gates)

    def test_v5_gate_contract_requires_exact_maturity_placement(self) -> None:
        names = (
            "Cross-client skill discovery", "Build workspace",
            "DBT virtual identity and pidfd_open policy", "Backend maturity",
            "Test regular workspace cases", "Documentation tests", "Clippy", "Rustfmt",
        )
        self.assertEqual(reverie_node.V5_GATES, names)
        gates = [{"name": name} for name in names]
        self.assertEqual(
            reverie_node.require_expected_gate_set(gates),
            ("reverie-local-validation/v5", names, reverie_node.V3_TEST_GATES),
        )
        invalid = {
            "duplicate maturity": gates[:4] + [gates[3]] + gates[4:],
            "late maturity": gates[:3] + gates[4:] + [gates[3]],
            "missing existing gate": gates[:4] + gates[5:],
            "v4 plus maturity": [{"name": name} for name in reverie_node.V4_GATES]
            + [gates[3]],
            "unknown gate": gates[:3] + [{"name": "Other maturity"}] + gates[4:],
        }
        for label, rows in invalid.items():
            with self.subTest(label=label), self.assertRaisesRegex(
                reverie_node.Refusal, "gate set drifted"
            ):
                reverie_node.require_expected_gate_set(rows)

    def test_safe_ci_cgroup_refuses_unboxed_and_accepts_step_scope(self) -> None:
        with mock.patch.object(Path, "read_text", return_value="0::/user.slice/agent.scope\n"):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node.safe_ci_cgroup()
        boxed_paths = [
            (
                "/user.slice/user-212630.slice/user@212630.service/safe.slice/"
                "safe-ci.slice/safe-ci-123.scope/step-validate.reverie-full"
            ),
            (
                "/user.slice/user-212630.slice/user@212630.service/dagrun.slice/"
                "dagrun-153463.scope/step-validate.reverie-full"
            ),
        ]
        for path in boxed_paths:
            with self.subTest(path=path):
                with mock.patch.object(Path, "read_text", return_value=f"0::{path}\n"):
                    self.assertEqual(reverie_node.safe_ci_cgroup(), path)

        refused_paths = [
            "/user.slice/safe.slice/safe-ci.slice/unsafe-ci-123.scope/step-validate.reverie-full",
            "/user.slice/safe.slice/safe-ci.slice/safe-ci-worker.scope/step-validate.reverie-full",
            "/user.slice/safe.slice/safe-ci.slice/safe-ci-123.scope/nested/step-validate.reverie-full",
            "/user.slice/dagrun.slice/worker-153463.scope/step-validate.reverie-full",
            "/user.slice/dagrun-153463.scope/step-validate.reverie-full",
            "/user.slice/dagrun.slice/dagrun-153463.scope/nested/step-validate.reverie-full",
            "/user.slice/dagrun.slice/dagrun-153463.scope/step-validate.other",
            "/user.slice//dagrun.slice/dagrun-153463.scope/step-validate.reverie-full",
        ]
        for path in refused_paths:
            with self.subTest(path=path):
                with mock.patch.object(Path, "read_text", return_value=f"0::{path}\n"):
                    with self.assertRaises(reverie_node.Refusal):
                        reverie_node.safe_ci_cgroup()

    def test_kvm_requirement_refuses_absence_and_accepts_exact_value(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node.require_kvm_environment()
        with mock.patch.dict(os.environ, {"REVERIE_REQUIRE_KVM": "1"}, clear=True):
            reverie_node.require_kvm_environment()


if __name__ == "__main__":
    unittest.main()
