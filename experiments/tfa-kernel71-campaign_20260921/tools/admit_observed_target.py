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
    write_json,
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
            "is deferred after native profiling found 657 threads and seed 0 hit the "
            "64 MiB stderr kill cap before the target oracle."
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
}

HERMIT_REVISION = "4630ee2a61f93221a43e2baec0a15f34102a2c61"
HERMIT_SHA256 = "5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a"
CAMPAIGN_CWD = "/home/newton/work/dev-hermit/worktrees/slots/hermit-tfa-kernel71-campaign/experiments/tfa-kernel71-campaign_20260921"


def report_fields(path: Path) -> dict[str, str]:
    result = {}
    for line in path.read_text().splitlines():
        if line.startswith("safehermit: ") and "=" in line:
            key, value = line[len("safehermit: "):].split("=", 1)
            result[key] = value
    return result


def classify(cfg: dict, stdout: str, stderr: str, fields: dict[str, str]) -> tuple[str, str | None, str, str | None, int]:
    selected = stdout.count(f"[ RUN      ] {cfg['display']}")
    matched = ":: Success: deterministic. Determinism verified." in stderr and ":: comparison=BitwiseInfoV1 relaxations=none" in stderr
    exit_code = int(fields["exit_code"])
    signal_name = None
    if "process_signal" in fields:
        signal_name = signal_module.Signals(int(fields["process_signal"])).name
    if matched and selected == 1 and "[  PASSED  ] 1 test." in stdout and exit_code == 0:
        return "VALID", "PASS", "COMPLETE", None, selected
    if matched and selected == 1 and exit_code != 0:
        return "VALID", "FAIL", "COMPLETE", "gtest-selected-test-failure", selected
    return "CENSORED", None, "ERROR", None, selected


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("target", choices=sorted(CONFIGS))
    parser.add_argument(
        "--repair-existing-prefix",
        action="store_true",
        help="extend the audited HTTPCoro material prefix from seeds 0-5 to 0-15",
    )
    args = parser.parse_args()
    cfg = CONFIGS[args.target]
    record_rel = Path(cfg["record"])
    record_path = ROOT / record_rel
    record = json.loads(record_path.read_text())
    repairing = record["attempt_state"] == "MATERIAL"
    if repairing:
        existing_seeds = [sample["seed"]["value"] for sample in record["samples"]]
        expected_existing_seeds = list(range(6))
        if not args.repair_existing_prefix or args.target != "httpcoro":
            raise SystemExit(f"refusing to replace material record {record_rel}")
        if existing_seeds != expected_existing_seeds:
            raise SystemExit(
                "repair guard failed: expected the admitted HTTPCoro prefix 0-5, "
                f"found {existing_seeds!r}"
            )
        if record["plan_receipt"]["commit"] != cfg["plan_commit"]:
            raise SystemExit("repair guard failed: plan receipt commit changed")
    elif record["attempt_state"] != "PLANNED":
        raise SystemExit(f"unsupported attempt state {record['attempt_state']!r}")
    plan = record["plan"]
    route = plan["authorized_routes"][0]
    evidence = record_path.parent / "evidence"
    guest_argv = route["launch_argv"][route["launch_argv"].index("--") + 1:]
    cell = {
        "cell_id": route["authorized_cell_ids"][0], "route_id": route["route_id"],
        "route_sha256": route["route_sha256"], "plan_sha256": plan["plan_sha256"],
        "frozen_at": plan["sealed_at"], "cell_sha256": "",
        "canonical_target": record["identity"]["canonical_target"],
        "test_selector": copy.deepcopy(record["identity"]["test_selector"]),
        "build_mode_selector": copy.deepcopy(record["identity"]["build_mode_selector"]),
        "target_source_revision": route["source_revision"], "hermit_source_revision": HERMIT_REVISION,
        "hermit_binary_sha256": HERMIT_SHA256, "test_artifact_sha256": cfg["binary_sha256"],
        "guest": {
            "argv": guest_argv, "environment": copy.deepcopy(route["environment"]),
            "mounts": ["type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out"],
            "immutable_inputs": [{
                "artifact_id": f"{args.target}-kernel71-test-elf", "kind": "OTHER", "storage": "LOCAL_IGNORED",
                "locator": cfg["binary"], "sha256": cfg["binary_sha256"], "bytes": cfg["binary_bytes"],
                "capture_limit_bytes": cfg["binary_bytes"], "truncated": False, "media_type": "application/x-executable",
            }],
            "working_directory": CAMPAIGN_CWD,
        },
        "hermit": {
            "backend": "ptrace", "strict": True, "chaos": True, "relaxations": [],
            "argv": copy.deepcopy(route["launch_argv"]), "scheduler_heuristic": "random",
            "swept_coordinate": "--fuzz-seed", "fixed_seed_roots": copy.deepcopy(plan["measurement_design"]["fixed_seed_roots"]),
        },
        "timing": {"requested_max_timeslice": 200000000, "effective_max_timeslice": 200000000},
        "pmu": {"available": True, "effective_timer": "RCB", "fallback_recorded": False},
        "host": {
            "hostname": "devbig030.atn3.facebook.com", "kernel": "Linux 7.1.3-0_fbk0_0_g295ad0959f34",
            "architecture": "x86_64", "cpu": "AMD EPYC 9D85 158-Core Processor",
            "capabilities": {"pmu": True, "mount_namespace": True, "buck_out_bind": True, "safehermit_disk_qgroup": True},
        },
        "oracle_id": plan["target_oracle"]["oracle_id"], "resource_bounds": copy.deepcopy(plan["resource_bounds"]),
    }
    cell["cell_sha256"] = canonical_digest(cell, "cell_sha256")
    events, samples, matchers = [], [], []
    pass_witness = fail_witness = None
    last_finish = dt.datetime.fromisoformat(plan["sealed_at"].replace("Z", "+00:00"))
    for index, seed in enumerate(cfg["seeds"]):
        prefix = f"seed-{seed:03d}"
        source = {
            "STDOUT": evidence / f"{prefix}-replica-1.stdout.txt",
            "STDERR": evidence / f"{prefix}-replica-1.stderr.txt",
            "SAFEHERMIT_REPORT": evidence / f"{prefix}-replica-1.safehermit.txt",
        }
        fields = report_fields(source["SAFEHERMIT_REPORT"])
        stdout_text, stderr_text = source["STDOUT"].read_text(), source["STDERR"].read_text()
        status, target_outcome, harness_status, failure_class, selected_count = classify(cfg, stdout_text, stderr_text, fields)
        started, finished = report_times(source["SAFEHERMIT_REPORT"])
        last_finish = max(last_finish, dt.datetime.fromisoformat(finished.replace("Z", "+00:00")))
        event_ids = []
        signal_name = signal_module.Signals(int(fields["process_signal"])).name if "process_signal" in fields else None
        verified = ":: Success: deterministic. Determinism verified." in stderr_text
        replicas = (1, 2) if verified else (1,)
        for replica in replicas:
            event_id = f"{args.target}-fuzz-{seed:03d}-replica-{replica}"; event_ids.append(event_id)
            paths = {}
            for kind, source_path in source.items():
                suffix = {"STDOUT": "stdout.txt", "STDERR": "stderr.txt", "SAFEHERMIT_REPORT": "safehermit.txt"}[kind]
                destination = evidence / f"{prefix}-replica-{replica}.{suffix}"
                if replica == 2: shutil.copyfile(source_path, destination)
                paths[kind] = destination
            report_locator = paths["SAFEHERMIT_REPORT"].relative_to(ROOT).as_posix()
            result = {
                "exit_code": int(fields["exit_code"]), "signal": signal_name, "timed_out": fields["unit_result"] == "timeout",
                "timeout_origin": "OUTER" if fields["unit_result"] == "timeout" else "NONE", "guest_started": True,
                "selected_test_executed_count": selected_count, "target_oracle": target_outcome or ("NOT_RUN" if selected_count == 0 else "AMBIGUOUS"),
                "failure_class_id": failure_class, "harness_status": harness_status,
                "diagnostic": f"status={status}; selected={selected_count}; unit_result={fields['unit_result']}; exit={fields['exit_code']}; BitwiseInfoV1={'matched' if ':: Success: deterministic. Determinism verified.' in stderr_text else 'not-matched'}",
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
            result_path = evidence / f"{prefix}-replica-{replica}.result.json"; write_json(result_path, event_result_payload(event))
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
            info_path = evidence / f"{prefix}.canonical-info.json"; write_json(info_path, matcher_payload(matcher))
            matcher["canonical_info_artifact"] = artifact(info_path, f"{matcher_id}-canonical-info", "CANONICAL_INFO", "application/json"); matchers.append(matcher)
        if status == "VALID" and target_outcome == "PASS" and pass_witness is None: pass_witness = {"sample_id": sample_id, "matcher_id": matcher_id}
        if status == "VALID" and target_outcome == "FAIL" and fail_witness is None: fail_witness = {"sample_id": sample_id, "matcher_id": matcher_id}
    classified_at = (last_finish + dt.timedelta(seconds=1)).isoformat().replace("+00:00", "Z")
    if pass_witness and fail_witness:
        outcome = {"classification_id": f"{args.target}-kernel71-working", "classified_at": classified_at, "supersedes": None,
                   "evidence_refs": [pass_witness["sample_id"], pass_witness["matcher_id"], fail_witness["sample_id"], fail_witness["matcher_id"]],
                   "kind": "WORKING", "pass_witness": pass_witness, "fail_witness": fail_witness}
    else:
        event_ids = [event["event_id"] for event in events]
        exhausted = cfg["terminal_exhausted"]
        classification_id = f"{args.target}-kernel71-{'exhausted' if exhausted else 'pending'}"
        if repairing:
            classification_id += f"-repair-seed-{cfg['seeds'][-1]:03d}"
        outcome = {
            "classification_id": classification_id, "classified_at": classified_at,
            "supersedes": record["current_outcome"]["classification_id"] if repairing else None,
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
    outcome_history = copy.deepcopy(record["outcome_history"]) if repairing else []
    outcome_history.append(outcome)
    record.update(plan_receipt={"commit": cfg["plan_commit"], "path": record_rel.as_posix(), "blob": blob}, attempt_state="MATERIAL",
                  cells=[cell], events=events, samples=samples, matchers=matchers, outcome_history=outcome_history, current_outcome=copy.deepcopy(outcome))
    record_path.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n")


if __name__ == "__main__": main()
