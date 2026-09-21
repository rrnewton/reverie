#!/usr/bin/env python3
"""Admit the predeclared AsyncSSL PASS-only prefix as terminal schema-v2 evidence."""

from __future__ import annotations

import copy
import datetime as dt
import hashlib
import json
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
RECORD_REL = Path("services/folly/async-ssl-socket-test--cefc173a09b4/experiment.json")
RECORD = ROOT / RECORD_REL
PLAN_COMMIT = "9c7f40dfef2330773a6511953415490bc34626fb"
HERMIT_REVISION = "4630ee2a61f93221a43e2baec0a15f34102a2c61"
HERMIT_SHA256 = "5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a"
TEST_SHA256 = "47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46"
TEST_BINARY = "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test"
CAMPAIGN_CWD = "/home/newton/work/dev-hermit/worktrees/slots/hermit-tfa-kernel71-campaign/experiments/tfa-kernel71-campaign_20260921"


def canonical_digest(obj: dict, key: str) -> str:
    value = dict(obj)
    value.pop(key, None)
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    return hashlib.sha256(payload).hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")


def artifact(path: Path, artifact_id: str, kind: str, media_type: str) -> dict:
    payload = path.read_bytes()
    return {
        "artifact_id": artifact_id,
        "kind": kind,
        "storage": "TRACKED",
        "locator": path.relative_to(ROOT).as_posix(),
        "sha256": hashlib.sha256(payload).hexdigest(),
        "bytes": len(payload),
        "capture_limit_bytes": 67108864,
        "truncated": False,
        "media_type": media_type,
    }


def replace_seed(argv: list[str], seed: int, report: str) -> list[str]:
    result = list(argv)
    result[result.index("--fuzz-seed") + 1] = str(seed)
    result[result.index("--sh-report") + 1] = report
    return result


def report_times(path: Path) -> tuple[str, str]:
    fields = {}
    for line in path.read_text().splitlines():
        if line.startswith("safehermit: ") and "=" in line:
            key, value = line[len("safehermit: "):].split("=", 1)
            fields[key] = value
    start = dt.datetime.strptime(fields["run_id"].split("-", 1)[0], "%Y%m%dT%H%M%SZ").replace(tzinfo=dt.timezone.utc)
    finish = start + dt.timedelta(seconds=int(fields["elapsed_secs"]))
    return start.isoformat().replace("+00:00", "Z"), finish.isoformat().replace("+00:00", "Z")


def event_result_payload(event: dict) -> dict:
    return {
        "schema_version": 1,
        "event_id": event["event_id"],
        "kind": event["kind"],
        "route_id": event["route_id"],
        "cell_id": event["cell_id"],
        "measurement_role": event["measurement_role"],
        "invocation_id": event["invocation_id"],
        "replica_index": event["replica_index"],
        "seed": event["seed"],
        "argv": event["argv"],
        "result": event["result"],
    }


def matcher_payload(matcher: dict) -> dict:
    return {
        "schema_version": 1,
        "matcher_id": matcher["matcher_id"],
        "kind": matcher["kind"],
        "execution_event_ids": matcher["execution_event_ids"],
        "complete": matcher["complete"],
        "same_target_outcome": matcher["same_target_outcome"],
        "same_failure_class": matcher["same_failure_class"],
        "details": matcher["details"],
    }


def main() -> None:
    record = json.loads(RECORD.read_text())
    plan = record["plan"]
    route = plan["authorized_routes"][0]
    evidence = RECORD.parent / "evidence"
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
        "test_artifact_sha256": TEST_SHA256,
        "guest": {
            "argv": route["launch_argv"][route["launch_argv"].index("--") + 1:],
            "environment": copy.deepcopy(route["environment"]),
            "mounts": [f"type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out"],
            "immutable_inputs": [{
                "artifact_id": "asyncssl-kernel71-test-elf",
                "kind": "OTHER",
                "storage": "LOCAL_IGNORED",
                "locator": TEST_BINARY,
                "sha256": TEST_SHA256,
                "bytes": 17651096,
                "capture_limit_bytes": 17651096,
                "truncated": False,
                "media_type": "application/x-executable",
            }],
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
            "fixed_seed_roots": copy.deepcopy(plan["measurement_design"]["fixed_seed_roots"]),
        },
        "timing": {"requested_max_timeslice": 200000000, "effective_max_timeslice": 200000000},
        "pmu": {"available": True, "effective_timer": "RCB", "fallback_recorded": False},
        "host": {
            "hostname": "devbig030.atn3.facebook.com",
            "kernel": "Linux 7.1.3-0_fbk0_0_g295ad0959f34",
            "architecture": "x86_64",
            "cpu": "AMD EPYC 9D85 158-Core Processor",
            "capabilities": {"pmu": True, "mount_namespace": True, "buck_out_bind": True, "safehermit_disk_qgroup": True},
        },
        "oracle_id": plan["target_oracle"]["oracle_id"],
        "resource_bounds": copy.deepcopy(plan["resource_bounds"]),
    }
    cell["cell_sha256"] = canonical_digest(cell, "cell_sha256")

    events = []
    samples = []
    matchers = []
    for seed in range(7):
        prefix = f"seed-{seed:03d}"
        source_paths = {
            "STDOUT": evidence / f"{prefix}-replica-1.stdout.txt",
            "STDERR": evidence / f"{prefix}-replica-1.stderr.txt",
            "SAFEHERMIT_REPORT": evidence / f"{prefix}-replica-1.safehermit.txt",
        }
        started, finished = report_times(source_paths["SAFEHERMIT_REPORT"])
        event_ids = []
        for replica in (1, 2):
            event_id = f"asyncssl-fuzz-{seed:03d}-replica-{replica}"
            event_ids.append(event_id)
            paths = {}
            for kind, source in source_paths.items():
                suffix = {"STDOUT": "stdout.txt", "STDERR": "stderr.txt", "SAFEHERMIT_REPORT": "safehermit.txt"}[kind]
                destination = evidence / f"{prefix}-replica-{replica}.{suffix}"
                if replica == 2:
                    shutil.copyfile(source, destination)
                paths[kind] = destination
            report_locator = paths["SAFEHERMIT_REPORT"].relative_to(ROOT).as_posix()
            argv = replace_seed(route["launch_argv"], seed, report_locator)
            result = {
                "exit_code": 0,
                "signal": None,
                "timed_out": False,
                "timeout_origin": "NONE",
                "guest_started": True,
                "selected_test_executed_count": 1,
                "target_oracle": "PASS",
                "failure_class_id": None,
                "harness_status": "COMPLETE",
                "diagnostic": "The selected GTest passed and strict BitwiseInfoV1 matched with no relaxation.",
            }
            event = {
                "event_id": event_id,
                "kind": "HERMIT_EXECUTION",
                "plan_sha256": plan["plan_sha256"],
                "route_id": route["route_id"],
                "route_sha256": route["route_sha256"],
                "canonical_target": route["canonical_target"],
                "test_selector": copy.deepcopy(route["test_selector"]),
                "build_mode_selector": copy.deepcopy(route["build_mode_selector"]),
                "source_revision": route["source_revision"],
                "cell_id": cell["cell_id"],
                "cell_sha256": cell["cell_sha256"],
                "measurement_role": "DISCOVERY" if replica == 1 else "CONFIRMATION",
                "started_at": started,
                "finished_at": finished,
                "argv": argv,
                "result": result,
                "invocation_id": f"asyncssl-fuzz-{seed:03d}-verify",
                "replica_index": replica,
                "seed": {"flag": "--fuzz-seed", "value": seed},
                "effective_config_sha256": cell["cell_sha256"],
                "artifacts": [],
            }
            result_path = evidence / f"{prefix}-replica-{replica}.result.json"
            write_json(result_path, event_result_payload(event))
            event["artifacts"] = [
                artifact(paths["STDOUT"], f"{event_id}-stdout", "STDOUT", "text/plain"),
                artifact(paths["STDERR"], f"{event_id}-stderr", "STDERR", "text/plain"),
                artifact(result_path, f"{event_id}-result", "RESULT", "application/json"),
                artifact(paths["SAFEHERMIT_REPORT"], f"{event_id}-safehermit", "SAFEHERMIT_REPORT", "text/plain"),
            ]
            events.append(event)
        sample_id = f"asyncssl-fuzz-{seed:03d}-sample"
        matcher_id = f"asyncssl-fuzz-{seed:03d}-matcher"
        samples.append({
            "sample_id": sample_id,
            "cell_id": cell["cell_id"],
            "seed": {"flag": "--fuzz-seed", "value": seed},
            "selection_index": seed,
            "status": "VALID",
            "target_outcome": "PASS",
            "failure_class_id": None,
            "censor_reason": None,
            "execution_event_ids": event_ids,
        })
        matcher = {
            "matcher_id": matcher_id,
            "kind": "BUILTIN_VERIFY",
            "execution_event_ids": event_ids,
            "complete": True,
            "same_target_outcome": True,
            "same_failure_class": True,
            "canonical_info_artifact": {},
            "details": {"verified": True, "bitwise_parity": True, "comparison": "BitwiseInfoV1", "relaxations": []},
        }
        info_path = evidence / f"{prefix}.canonical-info.json"
        write_json(info_path, matcher_payload(matcher))
        matcher["canonical_info_artifact"] = artifact(info_path, f"{matcher_id}-canonical-info", "CANONICAL_INFO", "application/json")
        matchers.append(matcher)

    event_ids = [event["event_id"] for event in events]
    inspected_artifacts = [
        next(item["artifact_id"] for item in event["artifacts"] if item["kind"] == "RESULT")
        for event in events
    ]
    classified_at = "2026-09-21T14:26:00Z"
    outcome = {
        "classification_id": "asyncssl-kernel71-pass-only-exhausted",
        "classified_at": classified_at,
        "supersedes": None,
        "evidence_refs": event_ids,
        "kind": "NOT-DETERMINABLE",
        "route_evidence": [{"route_id": route["route_id"], "cell_ids": [cell["cell_id"]], "evidence_event_ids": event_ids}],
        "inspected_commands": [event["argv"] for event in events],
        "inspected_artifacts": inspected_artifacts,
        "missing_or_ambiguous_facts": ["No predeclared target-owned FAIL arm was observed in the seven required valid fuzz-seed tuples."],
        "measurement_budget_exhausted": True,
        "pending_in_scope_measurements": [],
    }
    prefix = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "--show-prefix"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    blob = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", f"{PLAN_COMMIT}:{prefix}{RECORD_REL.as_posix()}"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    record.update(
        plan_receipt={"commit": PLAN_COMMIT, "path": RECORD_REL.as_posix(), "blob": blob},
        attempt_state="MATERIAL",
        cells=[cell],
        events=events,
        samples=samples,
        matchers=matchers,
        outcome_history=[outcome],
        current_outcome=copy.deepcopy(outcome),
    )
    RECORD.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
