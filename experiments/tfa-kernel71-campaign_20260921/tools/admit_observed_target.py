#!/usr/bin/env python3
"""Admit one initially PLANNED kernel-7.1 record from retained verifier runs."""

from __future__ import annotations

import argparse
import copy
import datetime as dt
import json
import re
import shutil
import signal as signal_module
import subprocess
from pathlib import Path

from admit_asyncssl_exhaustion import (
    ROOT,
    artifact,
    canonical_digest,
    event_result_payload,
    matcher_payload,
    report_times,
    replace_seed,
)


CONFIGS = {
    "timerfd": {
        "record": "services/folly/timerfd-timeout-manager-test--246a62382461/experiment.json",
        "plan_commit": "88915788933b31d6c65d3b54383016326aad9ab2",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/0a1a6e1cc8764b0f/folly/io/async/test/__timerfd_timeout_manager_test__/timerfd_timeout_manager_test",
        "binary_sha256": "f1e2b15c30e0b32e05378b5be9f2d59032bb00c1108f1bd2005aa9cb68692be7",
        "binary_bytes": 8743112,
        "display": "TimerFDTimeoutManagerTest.ReschedTest",
        "seeds": list(range(9)),
        "terminal_exhausted": True,
    },
    "httpcoro": {
        "record": "services/proxygen/httpcorosessiontests--e3f69b337304/experiment.json",
        "plan_commit": "cbfa263ec100079bad45c0c95b2beab241c97bca",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests",
        "binary_sha256": "00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc",
        "binary_bytes": 48908168,
        "display": "HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2",
        "seeds": list(range(16)),
        "terminal_exhausted": False,
        "pending_measurement": (
            "Seeds 16-99 remain in the predeclared population, but further HTTPCoro "
            "execution is deferred after causal analysis identified a single-thread "
            "slowdown/event-loop-budget mechanism rather than scheduler sensitivity."
        ),
    },
    "tls": {
        "record": "services/proxygen/tls-terminator-test--60719afd988e/experiment.json",
        "plan_commit": "fee8d8629972a8f69b15527901770d961bece0c3",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/49bed21049d8ec65/proxygen/facebook/fwdproxy/tlsterm/test/__tls_terminator_test__/tls_terminator_test",
        "binary_sha256": "e7a5278312bb9daa6d38a9a03cc2001cd31f8c64a92791d92d5aa47c20f66566",
        "binary_bytes": 991876712,
        "display": "TlsTerminatorTest/TlsTerminatorTest.terminateTls/h1_innerTls_http_connect",
        "seeds": list(range(15)),
        "terminal_exhausted": True,
    },
    "logdevice": {
        "record": "services/logdevice/tests--679930c8e36e/experiment.json",
        "plan_commit": "46e66de70ffe838bfdac67414bd055343b9f6e6f",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/240281cee994f93c/logdevice/admin/health_monitor/__tests__/tests",
        "binary_sha256": "ab45a7bcbfa86b5a22f563b3bcd6f80a90ce08b122357956418b030375e0157e",
        "binary_bytes": 2251301960,
        "display": "LogStoreHealthMonitorTest.DisabledRebuilding",
        "seeds": [0],
        "terminal_exhausted": False,
        "pending_measurement": (
            "Seeds 1-11 remain in the predeclared population, but further execution "
            "is deferred after native profiling found 657 threads and seed 0 ended "
            "in a two-thread scheduler deadlock and internal container failure before "
            "the target oracle; safehermit retained 966 bytes without truncation."
        ),
    },
    "cachelib": {
        "record": "services/cachelib/event-tracker-test--4b6700e9a3a0/experiment.json",
        "plan_commit": "97a24de87530df0a21743e4159263a314a3bf735",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/0bb5f293dc275764/cachelib/common/tests/__event-tracker-test__/event-tracker-test",
        "binary_sha256": "0f5a542c22025ad80dfdb992e4a2befc3cfa367d827c434237baa7554e268066",
        "binary_bytes": 10134232,
        "display": "EventTrackerTest.NvmCacheWithEventTracker",
        "seeds": [0],
        "terminal_exhausted": False,
        "pending_measurement": (
            "Seeds 1-93 remain in the predeclared population, but further execution "
            "is deferred after the selected native test exceeded 13 seconds and "
            "formal seed 0 reached the 120-second outer timeout."
        ),
    },
    "logstore": {
        "record": "services/logdevice/log-store-test--589f359badef/experiment.json",
        "plan_commit": "a37349f778ca3bf63044a8142e301465705e6b97",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/bfa56033afdaaeb5/logdevice/logservice/server/log_store/test/__log_store_test__/log_store_test",
        "binary_sha256": "73282dba27129be8c390b39cb0ca07affc66287a1d6db1f5f38562e89ef80b6b",
        "binary_bytes": 23180776,
        "display": "LogStoreTest.StoreRecordCondWriteFailedSameEpoch_FallbackReadFailsAndRetries",
        "seeds": [0],
        "terminal_exhausted": False,
        "pending_measurement": (
            "Seeds 1-34 remain in the predeclared population, but further execution "
            "is deferred because the exact native case emitted no marker within 13 "
            "seconds and formal seed 0 failed in the loader before the target oracle."
        ),
    },
    "mcrefill": {
        "record": "services/mcrouter/mc-refill-route-wrapper-test--82eb51b9426d/experiment.json",
        "plan_commit": "a93e41964f084e0e96dd1198cb04de5bbaae4566",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/037d27793008077d/mcrouter/facebook/test/toaster_tests/__mc_refill_route_wrapper_test__/mc_refill_route_wrapper_test",
        "binary_sha256": "3eaa55b7765bee4a5dbf578a535e24438d757d76b0d3e0d8f0da5185ff29b369",
        "binary_bytes": 12366336,
        "display": "McRefillWrapperBucketized/0.refillOnMiss",
        "seeds": [0],
        "terminal_exhausted": False,
        "pending_measurement": (
            "Seeds 1-16 remain in the predeclared population, but further execution "
            "is deferred because the exact native case emitted no marker within 25 "
            "seconds and formal seed 0 failed in the loader before the target oracle."
        ),
    },
    "monitor": {
        "record": "services/proxygen/proxygen-transport-monitor-test--fcaf56433e7f/experiment.json",
        "plan_commit": "508c116d27b337db92e4b81a77876dc9c8b4f937",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/3b44333ee2282447/proxygen/facebook/lib/transport/monitor/test/__proxygen_transport_monitor_test__/proxygen_transport_monitor_test",
        "binary_sha256": "115eba5170eef7bc39a583a5c3a5df28fb6596ac075994d35a60eb1c399d43ac",
        "binary_bytes": 17396776,
        "display": "PopulateEdgeConnectionsLoggerTests/PopulateEdgeConnectionsLoggerTest.PopulateEdgeConnectionsLoggerEvent/without_response",
        "seeds": [0],
        "terminal_exhausted": False,
    },
    "thrift": {
        "record": "services/thrift/instrumentationtest--3e07b8fc0ca0/experiment.json",
        "plan_commit": "b819db93c2ea9e4ee8fbc49037fb752ea5fdd150",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest",
        "binary_sha256": "2ec5d2128aa5af3ae86d46659054dd72b64ef86a389cf89934901a2e094cd0fc",
        "binary_bytes": 11514064,
        "display": "RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest",
        "seeds": list(range(36)),
        "terminal_exhausted": False,
        "classifier": "thrift-instrumentation-snapshot-size-2",
        "pending_measurement": (
            "Continue the committed fuzz-seed prefix through seed 35 until both "
            "target arms are confirmed, 29 valid tuples exhaust the rate-bound "
            "design, or the 36-attempt cap is reached."
        ),
    },
}

HERMIT_REVISION = "4630ee2a61f93221a43e2baec0a15f34102a2c61"
HERMIT_SHA256 = "5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a"
HERMIT_BINARY = "/home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a"
SAFEHERMIT_LOG_ROOT = "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit"
CAMPAIGN_CWD = "/home/newton/work/dev-hermit/worktrees/slots/hermit-tfa-kernel71-campaign/experiments/tfa-kernel71-campaign_20260921"


def report_fields(path: Path) -> dict[str, str]:
    result = {}
    for line in path.read_text().splitlines():
        if line.startswith("safehermit: ") and "=" in line:
            key, value = line[len("safehermit: "):].split("=", 1)
            result[key] = value
    return result


def write_json_once_or_verify(path: Path, value: object, verify_only: bool) -> None:
    payload = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    if verify_only:
        if not path.is_file() or path.read_bytes() != payload:
            raise SystemExit(f"prior JSON artifact changed: {path}")
        return
    if path.exists():
        raise SystemExit(f"refusing to overwrite new JSON artifact: {path}")
    path.write_bytes(payload)


def copy_once_or_verify(source: Path, destination: Path, verify_only: bool) -> None:
    if verify_only:
        if not destination.is_file() or destination.read_bytes() != source.read_bytes():
            raise SystemExit(f"prior replica artifact changed: {destination}")
        return
    if destination.exists():
        raise SystemExit(f"refusing to overwrite new replica artifact: {destination}")
    shutil.copyfile(source, destination)


def thrift_prior_prefix(
    record: dict, selected_seeds: list[int], plan_commit: str
) -> list[int]:
    current = record["current_outcome"]
    if current["kind"] != "NOT-DETERMINABLE":
        raise SystemExit(
            "refusing to extend a record whose outcome is not NOT-DETERMINABLE"
        )
    if current.get("measurement_budget_exhausted"):
        raise SystemExit("refusing to extend a terminal exhausted record")
    if record["plan_receipt"]["commit"] != plan_commit:
        raise SystemExit("repair guard failed: plan receipt commit changed")
    existing = [sample["seed"]["value"] for sample in record["samples"]]
    expected = selected_seeds[:-1]
    if existing != expected:
        raise SystemExit(
            f"repair guard failed: expected prior admitted prefix {expected!r}, "
            f"found {existing!r}"
        )
    return existing


def assert_prior_prefix_unchanged(
    record: dict,
    cell: dict,
    events: list[dict],
    samples: list[dict],
    matchers: list[dict],
) -> None:
    comparisons = (
        (record["cells"], [cell], "frozen cell"),
        (record["events"], events[: len(record["events"])], "execution events"),
        (record["samples"], samples[: len(record["samples"])], "samples"),
        (record["matchers"], matchers[: len(record["matchers"])], "matchers"),
    )
    for prior, rebuilt, label in comparisons:
        if prior != rebuilt:
            raise SystemExit(f"prior {label} changed during prefix reconstruction")


def preserved_non_hermit_event_prefix(record: dict, repairing: bool) -> list[dict]:
    """Keep pre-cell launch diagnostics while rebuilding Hermit seed events."""
    if not repairing:
        return []
    events = record["events"]
    first_hermit = next(
        (
            index
            for index, event in enumerate(events)
            if event["kind"] == "HERMIT_EXECUTION"
        ),
        len(events),
    )
    if any(event["kind"] != "HERMIT_EXECUTION" for event in events[first_hermit:]):
        raise SystemExit(
            "repair guard failed: non-Hermit event appears after Hermit seed evidence"
        )
    return copy.deepcopy(events[:first_hermit])


def measurement_budget_exhausted(design: dict, samples: list[dict]) -> bool:
    valid_count = sum(sample["status"] == "VALID" for sample in samples)
    return (
        valid_count >= design["required_valid_seed_tuples"]
        or len(samples) >= design["max_attempted_seed_tuples"]
    )


def supersedes_id(record: dict, repairing: bool) -> str | None:
    return record["current_outcome"]["classification_id"] if repairing else None


def assert_first_thrift_admission(repairing: bool, selected_seeds: list[int]) -> None:
    if not repairing and selected_seeds != [0]:
        raise SystemExit("the first Thrift admission must contain only sealed seed 0")


def assert_plan_receipt_preserved(
    record: dict, receipt: dict, repairing: bool
) -> None:
    if repairing and record["plan_receipt"] != receipt:
        raise SystemExit("repair guard failed: exact plan receipt changed")


def infrastructure_failure_present(
    stdout: str, stderr: str, fields: dict[str, str]
) -> bool:
    combined = stdout + "\n" + stderr
    markers = (
        "HERMIT_INTERNAL_FAILURE",
        "Sandbox container exited unexpectedly",
        "class=container-child-exit",
        "error while loading shared libraries",
        "safehermit: byte limit reached",
    )
    return fields.get("truncated") == "true" or any(
        marker in combined for marker in markers
    )


def strict_bitwise_matched(stderr: str) -> bool:
    return (
        ":: Success: deterministic. Determinism verified." in stderr
        and ":: comparison=BitwiseInfoV1 relaxations=none" in stderr
    )


def formal_wrapper_boundary_applied(fields: dict[str, str]) -> bool:
    expected = {
        "binary": HERMIT_BINARY,
        "binary_sha256": HERMIT_SHA256,
        "bound.wall": "APPLIED:120s",
        "bound.cgroup": "APPLIED:MemoryMax=16G MemorySwapMax=0",
        "bound.disk": (
            "APPLIED:8G for log_dir only; guest paths and explicit log files "
            "elsewhere are outside this bound"
        ),
        "bound.bytes": (
            "APPLIED:67108864 (LETHAL: the run is cgroup-killed at the cap)"
        ),
        "truncated": "false",
    }
    log_prefix = f"{SAFEHERMIT_LOG_ROOT}/brs-safehermit-"
    return all(fields.get(key) == value for key, value in expected.items()) and fields.get(
        "log_dir", ""
    ).startswith(log_prefix)


def assert_formal_wrapper_boundary(fields: dict[str, str]) -> None:
    if not formal_wrapper_boundary_applied(fields):
        raise SystemExit(
            "formal wrapper boundary not applied exactly; refusing to create a seed sample"
        )


def classify_thrift_instrumentation(
    cfg: dict, stdout: str, stderr: str, fields: dict[str, str]
) -> tuple[str, str | None, str, str | None, int]:
    lines = stdout.splitlines()
    run_line = f"[ RUN      ] {cfg['display']}"
    selected = lines.count(run_line)
    run_indices = [index for index, line in enumerate(lines) if line.startswith("[ RUN      ] ")]
    selected_run_index = lines.index(run_line) if selected == 1 else None
    matched = strict_bitwise_matched(stderr)
    infra_free = not infrastructure_failure_present(stdout, stderr, fields)
    exit_code = int(fields["exit_code"])
    signal_name = None
    if "process_signal" in fields:
        signal_name = signal_module.Signals(int(fields["process_signal"])).name

    if fields["unit_result"] == "timeout":
        return "CENSORED", None, "TIMEOUT", None, selected

    one_test_line = "[==========] Running 1 test from 1 test suite."
    one_test_indices = [index for index, line in enumerate(lines) if line == one_test_line]
    ok_line = re.compile(
        rf"^\[       OK \] {re.escape(cfg['display'])} \([0-9]+ ms\)$"
    )
    ok_indices = [
        index for index, line in enumerate(lines) if ok_line.fullmatch(line) is not None
    ]
    passed_line = "[  PASSED  ] 1 test."
    passed_indices = [index for index, line in enumerate(lines) if line == passed_line]
    pass_signature = bool(
        selected_run_index is not None
        and len(ok_indices) == 1
        and len(passed_indices) == 1
        and selected_run_index < ok_indices[0] < passed_indices[0]
    )
    snapshot_lines = [
        "fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp:836: Failure",
        "Expected equality of these values:",
        "  reqSnapshots.size()",
        "    Which is: 2",
        "  3",
    ]
    snapshot_indices = [
        index
        for index in range(len(lines) - len(snapshot_lines) + 1)
        if lines[index : index + len(snapshot_lines)] == snapshot_lines
    ]
    snapshot_signature = bool(
        selected_run_index is not None
        and len(snapshot_indices) == 1
        and lines.count(snapshot_lines[0]) == 1
        and selected_run_index < snapshot_indices[0]
    )
    common = bool(
        matched
        and infra_free
        and formal_wrapper_boundary_applied(fields)
        and len(run_indices) == 1
        and selected_run_index is not None
        and len(one_test_indices) == 1
        and one_test_indices[0] < selected_run_index
    )
    complete_abnormal = (
        fields["unit_result"] in {"exit-code", "signal", "core-dump"}
        and (exit_code != 0 or signal_name is not None)
    )

    if (
        common
        and fields["unit_result"] == "success"
        and exit_code == 0
        and signal_name is None
        and pass_signature
        and not snapshot_signature
        and "WARNING: ThreadSanitizer:" not in stderr
    ):
        return "VALID", "PASS", "COMPLETE", None, selected
    if common and complete_abnormal and snapshot_signature:
        return "VALID", "FAIL", "COMPLETE", "gtest-snapshot-size-2", selected
    return "CENSORED", None, "ERROR", None, selected


def classify(cfg: dict, stdout: str, stderr: str, fields: dict[str, str]) -> tuple[str, str | None, str, str | None, int]:
    if cfg.get("classifier") == "thrift-instrumentation-snapshot-size-2":
        return classify_thrift_instrumentation(cfg, stdout, stderr, fields)

    selected = stdout.count(f"[ RUN      ] {cfg['display']}")
    matched = strict_bitwise_matched(stderr)
    exit_code = int(fields["exit_code"])
    if fields["unit_result"] == "timeout":
        return "CENSORED", None, "TIMEOUT", None, selected
    if matched and selected == 1 and "[  PASSED  ] 1 test." in stdout and exit_code == 0:
        return "VALID", "PASS", "COMPLETE", None, selected
    if matched and selected == 1 and exit_code != 0:
        return "VALID", "FAIL", "COMPLETE", "gtest-selected-test-failure", selected
    return "CENSORED", None, "ERROR", None, selected


def build_cell(record: dict, cfg: dict, target: str) -> dict:
    plan = record["plan"]
    route = plan["authorized_routes"][0]
    guest_argv = route["launch_argv"][route["launch_argv"].index("--") + 1 :]
    cell = {
        "cell_id": route["authorized_cell_ids"][0],
        "route_id": route["route_id"],
        "route_sha256": route["route_sha256"],
        "plan_sha256": plan["plan_sha256"],
        "frozen_at": plan["sealed_at"],
        "cell_sha256": "",
        "canonical_target": record["identity"]["canonical_target"],
        "test_selector": copy.deepcopy(record["identity"]["test_selector"]),
        "build_mode_selector": copy.deepcopy(record["identity"]["build_mode_selector"]),
        "target_source_revision": route["source_revision"],
        "hermit_source_revision": HERMIT_REVISION,
        "hermit_binary_sha256": HERMIT_SHA256,
        "test_artifact_sha256": cfg["binary_sha256"],
        "guest": {
            "argv": guest_argv,
            "environment": copy.deepcopy(route["environment"]),
            "mounts": [
                "type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out"
            ],
            "immutable_inputs": [
                {
                    "artifact_id": f"{target}-kernel71-test-elf",
                    "kind": "OTHER",
                    "storage": "LOCAL_IGNORED",
                    "locator": cfg["binary"],
                    "sha256": cfg["binary_sha256"],
                    "bytes": cfg["binary_bytes"],
                    "capture_limit_bytes": cfg["binary_bytes"],
                    "truncated": False,
                    "media_type": "application/x-executable",
                }
            ],
            "working_directory": CAMPAIGN_CWD,
        },
        "hermit": {
            "backend": "ptrace",
            "strict": True,
            "chaos": True,
            "relaxations": [],
            "argv": copy.deepcopy(route["launch_argv"]),
            "scheduler_heuristic": "random",
            "swept_coordinate": "--fuzz-seed",
            "fixed_seed_roots": copy.deepcopy(
                plan["measurement_design"]["fixed_seed_roots"]
            ),
        },
        "timing": {
            "requested_max_timeslice": 200000000,
            "effective_max_timeslice": 200000000,
        },
        "pmu": {
            "available": True,
            "effective_timer": "RCB",
            "fallback_recorded": False,
        },
        "host": {
            "hostname": "devbig030.atn3.facebook.com",
            "kernel": "Linux 7.1.3-0_fbk0_0_g295ad0959f34",
            "architecture": "x86_64",
            "cpu": "AMD EPYC 9D85 158-Core Processor",
            "capabilities": {
                "pmu": True,
                "mount_namespace": True,
                "buck_out_bind": True,
                "safehermit_disk_qgroup": True,
            },
        },
        "oracle_id": plan["target_oracle"]["oracle_id"],
        "resource_bounds": copy.deepcopy(plan["resource_bounds"]),
    }
    cell["cell_sha256"] = canonical_digest(cell, "cell_sha256")
    return cell


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("target", choices=sorted(CONFIGS))
    parser.add_argument(
        "--repair-existing-prefix",
        action="store_true",
        help="extend the audited HTTPCoro material prefix from seeds 0-5 to 0-15",
    )
    parser.add_argument(
        "--extend-existing-prefix",
        action="store_true",
        help="extend a supported material record by exactly one committed seed prefix",
    )
    parser.add_argument(
        "--through-seed",
        type=int,
        help="for Thrift instrumentation, admit the complete sealed prefix through this seed",
    )
    args = parser.parse_args()
    cfg = CONFIGS[args.target]
    if args.target == "thrift":
        if args.repair_existing_prefix:
            raise SystemExit("--repair-existing-prefix is reserved for HTTPCoro")
        if args.through_seed not in cfg["seeds"]:
            raise SystemExit("thrift requires --through-seed in the sealed range 0..35")
        selected_seeds = list(range(args.through_seed + 1))
    else:
        if args.through_seed is not None or args.extend_existing_prefix:
            raise SystemExit("prefix extension options are supported only for thrift")
        selected_seeds = cfg["seeds"]
    record_rel = Path(cfg["record"])
    record_path = ROOT / record_rel
    record = json.loads(record_path.read_text())
    repairing = record["attempt_state"] == "MATERIAL"
    if args.target == "thrift":
        assert_first_thrift_admission(repairing, selected_seeds)
    if not repairing and args.extend_existing_prefix:
        raise SystemExit("cannot extend a PLANNED record; admit its first seed normally")
    if repairing:
        if args.target == "httpcoro" and args.repair_existing_prefix:
            existing_seeds = [sample["seed"]["value"] for sample in record["samples"]]
            expected_existing_seeds = list(range(6))
        elif args.target == "thrift" and args.extend_existing_prefix:
            existing_seeds = thrift_prior_prefix(
                record, selected_seeds, cfg["plan_commit"]
            )
            expected_existing_seeds = existing_seeds
        else:
            raise SystemExit(f"refusing to replace material record {record_rel}")
        if existing_seeds != expected_existing_seeds:
            raise SystemExit(
                "repair guard failed: expected prior admitted prefix "
                f"{expected_existing_seeds!r}, found {existing_seeds!r}"
            )
        if record["plan_receipt"]["commit"] != cfg["plan_commit"]:
            raise SystemExit("repair guard failed: plan receipt commit changed")
    elif record["attempt_state"] != "PLANNED":
        raise SystemExit(f"unsupported attempt state {record['attempt_state']!r}")
    plan = record["plan"]
    route = plan["authorized_routes"][0]
    evidence = record_path.parent / "evidence"
    cell = build_cell(record, cfg, args.target)
    events = preserved_non_hermit_event_prefix(record, repairing)
    samples, matchers = [], []
    pass_witness = fail_witness = None
    last_finish = dt.datetime.fromisoformat(plan["sealed_at"].replace("Z", "+00:00"))
    for index, seed in enumerate(selected_seeds):
        prior_seed = repairing and index < len(existing_seeds)
        prefix = f"seed-{seed:03d}"
        source = {
            "STDOUT": evidence / f"{prefix}-replica-1.stdout.txt",
            "STDERR": evidence / f"{prefix}-replica-1.stderr.txt",
            "SAFEHERMIT_REPORT": evidence / f"{prefix}-replica-1.safehermit.txt",
        }
        fields = report_fields(source["SAFEHERMIT_REPORT"])
        if args.target == "thrift":
            assert_formal_wrapper_boundary(fields)
        stdout_text, stderr_text = source["STDOUT"].read_text(), source["STDERR"].read_text()
        status, target_outcome, harness_status, failure_class, selected_count = classify(cfg, stdout_text, stderr_text, fields)
        started, finished = report_times(source["SAFEHERMIT_REPORT"])
        last_finish = max(last_finish, dt.datetime.fromisoformat(finished.replace("Z", "+00:00")))
        event_ids = []
        signal_name = signal_module.Signals(int(fields["process_signal"])).name if "process_signal" in fields else None
        verified = strict_bitwise_matched(stderr_text)
        replicas = (1, 2) if verified else (1,)
        for replica in replicas:
            event_id = f"{args.target}-fuzz-{seed:03d}-replica-{replica}"; event_ids.append(event_id)
            paths = {}
            for kind, source_path in source.items():
                suffix = {"STDOUT": "stdout.txt", "STDERR": "stderr.txt", "SAFEHERMIT_REPORT": "safehermit.txt"}[kind]
                destination = evidence / f"{prefix}-replica-{replica}.{suffix}"
                if replica == 2:
                    copy_once_or_verify(source_path, destination, prior_seed)
                paths[kind] = destination
            report_locator = paths["SAFEHERMIT_REPORT"].relative_to(ROOT).as_posix()
            result = {
                "exit_code": int(fields["exit_code"]), "signal": signal_name, "timed_out": fields["unit_result"] == "timeout",
                "timeout_origin": "OUTER" if fields["unit_result"] == "timeout" else "NONE", "guest_started": True,
                "selected_test_executed_count": selected_count, "target_oracle": target_outcome or ("NOT_RUN" if selected_count == 0 else "AMBIGUOUS"),
                "failure_class_id": failure_class, "harness_status": harness_status,
                "diagnostic": f"status={status}; selected={selected_count}; unit_result={fields['unit_result']}; exit={fields['exit_code']}; BitwiseInfoV1={'matched' if verified else 'not-matched'}; formal_wrapper_boundary={'applied' if formal_wrapper_boundary_applied(fields) else 'not-applied'}",
            }
            event = {
                "event_id": event_id, "kind": "HERMIT_EXECUTION", "plan_sha256": plan["plan_sha256"],
                "route_id": route["route_id"], "route_sha256": route["route_sha256"], "canonical_target": route["canonical_target"],
                "test_selector": copy.deepcopy(route["test_selector"]), "build_mode_selector": copy.deepcopy(route["build_mode_selector"]),
                "source_revision": route["source_revision"], "cell_id": cell["cell_id"], "cell_sha256": cell["cell_sha256"],
                "measurement_role": "DISCOVERY" if replica == 1 else "CONFIRMATION", "started_at": started, "finished_at": finished,
                "argv": replace_seed(route["launch_argv"], seed, report_locator), "result": result,
                "invocation_id": f"{args.target}-fuzz-{seed:03d}-verify", "replica_index": replica,
                "seed": {"flag": "--fuzz-seed", "value": seed}, "effective_config_sha256": cell["cell_sha256"], "artifacts": [],
            }
            result_path = evidence / f"{prefix}-replica-{replica}.result.json"
            write_json_once_or_verify(result_path, event_result_payload(event), prior_seed)
            event["artifacts"] = [
                artifact(paths["STDOUT"], f"{event_id}-stdout", "STDOUT", "text/plain"),
                artifact(paths["STDERR"], f"{event_id}-stderr", "STDERR", "text/plain"),
                artifact(result_path, f"{event_id}-result", "RESULT", "application/json"),
                artifact(paths["SAFEHERMIT_REPORT"], f"{event_id}-safehermit", "SAFEHERMIT_REPORT", "text/plain"),
            ]; events.append(event)
        sample_id, matcher_id = f"{args.target}-fuzz-{seed:03d}-sample", f"{args.target}-fuzz-{seed:03d}-matcher"
        samples.append({
            "sample_id": sample_id, "cell_id": cell["cell_id"], "seed": {"flag": "--fuzz-seed", "value": seed},
            "selection_index": index, "status": status, "target_outcome": target_outcome, "failure_class_id": failure_class,
            "censor_reason": None if status == "VALID" else "The selected test did not produce a qualifying target verdict.",
            "execution_event_ids": event_ids,
        })
        if verified:
            matcher = {
                "matcher_id": matcher_id, "kind": "BUILTIN_VERIFY", "execution_event_ids": event_ids, "complete": True,
                "same_target_outcome": True, "same_failure_class": True, "canonical_info_artifact": {},
                "details": {"verified": True, "bitwise_parity": True, "comparison": "BitwiseInfoV1", "relaxations": []},
            }
            info_path = evidence / f"{prefix}.canonical-info.json"
            write_json_once_or_verify(info_path, matcher_payload(matcher), prior_seed)
            matcher["canonical_info_artifact"] = artifact(info_path, f"{matcher_id}-canonical-info", "CANONICAL_INFO", "application/json"); matchers.append(matcher)
        if status == "VALID" and target_outcome == "PASS" and pass_witness is None: pass_witness = {"sample_id": sample_id, "matcher_id": matcher_id}
        if status == "VALID" and target_outcome == "FAIL" and fail_witness is None: fail_witness = {"sample_id": sample_id, "matcher_id": matcher_id}
    if repairing:
        assert_prior_prefix_unchanged(record, cell, events, samples, matchers)
    classified_at = (last_finish + dt.timedelta(seconds=1)).isoformat().replace("+00:00", "Z")
    if pass_witness and fail_witness:
        outcome = {"classification_id": f"{args.target}-kernel71-working", "classified_at": classified_at,
                   "supersedes": supersedes_id(record, repairing),
                   "evidence_refs": [pass_witness["sample_id"], pass_witness["matcher_id"], fail_witness["sample_id"], fail_witness["matcher_id"]],
                   "kind": "WORKING", "pass_witness": pass_witness, "fail_witness": fail_witness}
    else:
        event_ids = [event["event_id"] for event in events]
        if args.target == "thrift":
            exhausted = measurement_budget_exhausted(
                plan["measurement_design"], samples
            )
        else:
            exhausted = cfg["terminal_exhausted"]
        classification_id = f"{args.target}-kernel71-{'exhausted' if exhausted else 'pending'}"
        if repairing:
            classification_id += f"-repair-seed-{selected_seeds[-1]:03d}"
        elif args.target == "thrift":
            classification_id += f"-through-seed-{selected_seeds[-1]:03d}"
        outcome = {
            "classification_id": classification_id, "classified_at": classified_at,
            "supersedes": supersedes_id(record, repairing),
            "evidence_refs": event_ids, "kind": "NOT-DETERMINABLE",
            "route_evidence": [{"route_id": route["route_id"], "cell_ids": [cell["cell_id"]], "evidence_event_ids": event_ids}],
            "inspected_commands": [event["argv"] for event in events],
            "inspected_artifacts": [next(a["artifact_id"] for a in event["artifacts"] if a["kind"] == "RESULT") for event in events],
            "missing_or_ambiguous_facts": ["A complete distinct-seed PASS/target-owned-FAIL bracket is not yet present."],
            "measurement_budget_exhausted": exhausted,
            "pending_in_scope_measurements": [] if exhausted else [cfg.get(
                "pending_measurement",
                "Continue the committed predeclared fuzz-seed prefix without changing the cell.",
            )],
        }
    prefix = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--show-prefix"], check=True, capture_output=True, text=True).stdout.strip()
    blob = subprocess.run(["git", "-C", str(ROOT), "rev-parse", f"{cfg['plan_commit']}:{prefix}{record_rel.as_posix()}"], check=True, capture_output=True, text=True).stdout.strip()
    receipt = {"commit": cfg["plan_commit"], "path": record_rel.as_posix(), "blob": blob}
    assert_plan_receipt_preserved(record, receipt, repairing)
    outcome_history = copy.deepcopy(record["outcome_history"]) if repairing else []
    outcome_history.append(outcome)
    record.update(plan_receipt=receipt, attempt_state="MATERIAL",
                  cells=[cell], events=events, samples=samples, matchers=matchers, outcome_history=outcome_history, current_outcome=copy.deepcopy(outcome))
    record_path.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n")


if __name__ == "__main__": main()
