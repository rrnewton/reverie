#!/usr/bin/env python3
"""Create the pre-run schema-v2 plans for the kernel-7.1 TFA cell."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SOURCE_REVISION = "7f239d5b37dfbecf32fed144468da461f998add5"
HERMIT_REVISION = "4630ee2a61f93221a43e2baec0a15f34102a2c61"
HERMIT_BINARY = "/home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a"
HERMIT_SHA256 = "5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a"
SEALED_AT = "2026-09-21T14:16:43Z"
HOST_KERNEL = "Linux 7.1.3-0_fbk0_0_g295ad0959f34"
HOSTNAME = "devbig030.atn3.facebook.com"
CPU = "AMD EPYC 9D85 158-Core Processor"
BUCK_OUT = "/home/newton/work/tfa/fbsource/buck-out"
CAMPAIGN_DIR = "/home/newton/work/dev-hermit/worktrees/slots/hermit-tfa-kernel71-campaign/experiments/tfa-kernel71-campaign_20260921"


CONFIGS = [
    {
        "name": "asyncssl",
        "target": "fbcode//folly/io/async/test:async_ssl_socket_test",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test",
        "binary_sha256": "47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46",
        "binary_bytes": 17651096,
        "gtest_filter": "AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect",
        "gtest_display": "AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect",
        "run_disabled": True,
        "required_n": 7,
        "attempt_cap": 11,
        "verify_dir": "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-asyncssl-verify-4145587-1790000154812521376",
    },
    {
        "name": "monitor",
        "target": "fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/3b44333ee2282447/proxygen/facebook/lib/transport/monitor/test/__proxygen_transport_monitor_test__/proxygen_transport_monitor_test",
        "binary_sha256": "115eba5170eef7bc39a583a5c3a5df28fb6596ac075994d35a60eb1c399d43ac",
        "binary_bytes": 17396776,
        "gtest_filter": "PopulateEdgeConnectionsLoggerTests/PopulateEdgeConnectionsLoggerTest.PopulateEdgeConnectionsLoggerEvent/without_response",
        "gtest_display": "PopulateEdgeConnectionsLoggerTests/PopulateEdgeConnectionsLoggerTest.PopulateEdgeConnectionsLoggerEvent/without_response",
        "run_disabled": False,
        "required_n": 44,
        "attempt_cap": 52,
        "verify_dir": "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-monitor-verify-4145718-1790000155029090422",
    },
    {
        "name": "httpcoro",
        "target": "fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests",
        "binary_sha256": "00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc",
        "binary_bytes": 48908168,
        "gtest_filter": "HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2",
        "gtest_display": "HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2",
        "run_disabled": False,
        "required_n": 89,
        "attempt_cap": 100,
        "verify_dir": "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-httpcoro-verify-4145838-1790000155205015507",
    },
    {
        "name": "timerfd",
        "target": "fbcode//folly/io/async/test:timerfd_timeout_manager_test",
        "binary": "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/0a1a6e1cc8764b0f/folly/io/async/test/__timerfd_timeout_manager_test__/timerfd_timeout_manager_test",
        "binary_sha256": "f1e2b15c30e0b32e05378b5be9f2d59032bb00c1108f1bd2005aa9cb68692be7",
        "binary_bytes": 8743112,
        "gtest_filter": "TimerFDTimeoutManagerTest.ReschedTest",
        "gtest_display": "TimerFDTimeoutManagerTest.ReschedTest",
        "run_disabled": True,
        "required_n": 9,
        "attempt_cap": 13,
        "verify_dir": "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-timerfd-verify-4146036-1790000155380805628",
    },
]


def digest(obj: dict, key: str) -> str:
    value = dict(obj)
    value.pop(key, None)
    payload = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    return hashlib.sha256(payload).hexdigest()


def make_plan(entry: dict, cfg: dict) -> dict:
    identity = {
        key: copy.deepcopy(entry[key])
        for key in (
            "service",
            "canonical_target",
            "target_key_sha256",
            "target_directory",
            "testinfra_id",
            "test_name",
            "source_label_qualifier",
            "test_selector",
            "build_mode_selector",
            "invoked_target",
            "standalone_argv",
        )
    }
    identity["experiment_id"] = f"{entry['service']}/{Path(entry['target_directory']).name}"
    identity["alias_resolution"] = {
        "input_target": entry["invoked_target"],
        "resolved_target": entry["canonical_target"],
        "method": "pinned-cohort-target-and-existing-buck-output",
        "source_revision": SOURCE_REVISION,
        "evidence": (
            f"The fixed cohort resolves this canonical target. Existing executable "
            f"{cfg['binary']} is {cfg['binary_bytes']} bytes with SHA-256 "
            f"{cfg['binary_sha256']}."
        ),
    }

    guest = [cfg["binary"], f"--gtest_filter={cfg['gtest_filter']}"]
    if cfg["run_disabled"]:
        guest.append("--gtest_also_run_disabled_tests")
    fixed_roots = {
        "--seed": 0,
        "--rng-seed": 0,
        "--sched-seed": 0,
        "--fuzz-seed": None,
    }
    bounds = {
        "wall_seconds": 120,
        "memory_bytes": 17179869184,
        "swap_bytes": 0,
        "stderr_bytes": 67108864,
        "retained_log_bytes": 67108864,
        "disk_guard": "8G",
    }
    launch = [
        "bin/safehermit",
        "--sh-report",
        "{SAFEHERMIT_REPORT}",
        "--sh-deadline",
        str(bounds["wall_seconds"]),
        "--sh-max-log-bytes",
        str(bounds["stderr_bytes"]),
        "--sh-disk-limit",
        bounds["disk_guard"],
        HERMIT_BINARY,
        "run",
        "--backend",
        "ptrace",
        "--strict",
        "--chaos",
        "--chaos-target-races",
        "--base-env=minimal",
        "--env",
        "LC_ALL=C",
        "--env",
        "TZ=UTC",
        f"--mount=type=bind,source={BUCK_OUT},target={BUCK_OUT}",
        "--seed",
        "0",
        "--rng-seed",
        "0",
        "--sched-seed",
        "0",
        "--fuzz-seed",
        "{SWEEP}",
        "--sched-heuristic",
        "random",
        "--max-timeslice",
        "200000000",
        "--verify",
        "--verify-strict",
        "--verify-allow",
        "both",
        "--keep-logs",
        f"--verify-log-dir={cfg['verify_dir']}",
        "--",
        *guest,
    ]
    cell_id = f"{cfg['name']}-kernel71-ptrace-strict-chaos-target-races"
    route = {
        "route_id": f"{cfg['name']}-kernel71-existing-elf",
        "route_sha256": "",
        "fallback_of": None,
        "authorized_cell_ids": [cell_id],
        "canonical_target": entry["canonical_target"],
        "test_selector": copy.deepcopy(entry["test_selector"]),
        "build_mode_selector": copy.deepcopy(entry["build_mode_selector"]),
        "source_revision": SOURCE_REVISION,
        "build_argv": copy.deepcopy(entry["standalone_argv"]),
        "launch_argv": launch,
        "repeat_matcher": {
            "kind": "BUILTIN_VERIFY",
            "comparison": "BitwiseInfoV1",
        },
        "environment": {"LC_ALL": "C", "TZ": "UTC"},
        "notes": (
            f"PRE-RUN SEAL: one authorized cell {cell_id}; Hermit revision "
            f"{HERMIT_REVISION}, binary SHA-256 {HERMIT_SHA256}; test ELF "
            f"SHA-256 {cfg['binary_sha256']}; host {HOSTNAME}, kernel "
            f"{HOST_KERNEL}, x86_64 {CPU}; PMU type 4/RCB available; ptrace "
            f"strict chaos with --chaos-target-races; only --fuzz-seed varies; "
            f"BitwiseInfoV1 with no relaxations; verifier qgroup {cfg['verify_dir']}."
        ),
    }
    route["route_sha256"] = digest(route, "route_sha256")
    native = entry["candidate_ranking"]["native_failure_rate"]
    design = {
        "purpose": "EXISTENCE_DISCOVERY",
        "swept_coordinate": {
            "flag": "--fuzz-seed",
            "domain": f"unsigned 64-bit integers; explicit prefix 0 through {cfg['attempt_cap'] - 1}",
            "scheduler_modes": ["--chaos-target-races"],
        },
        "fixed_seed_roots": fixed_roots,
        "seed_population": {
            "kind": "EXPLICIT_VALUES",
            "values": list(range(cfg["attempt_cap"])),
        },
        "selection_order": {
            "kind": "EXPLICIT",
            "seed_values": list(range(cfg["attempt_cap"])),
        },
        "without_replacement": True,
        "design_basis": {
            "kind": "RATE_BOUND",
            "expected_hermit_fail_probability": native["rate"],
            "provenance_kind": "NATIVE_EMPIRICAL_RATE",
            "provenance": (
                f"The fixed cohort records {native['failures']} failures among "
                f"{native['executed']} final native outcomes. This is a "
                "non-equivalent planning prior only, not a Hermit rate claim."
            ),
            "equivalence_to_hermit": False,
            "max_miss_probability": 0.05,
        },
        "miss_probability_override": None,
        "required_valid_seed_tuples": cfg["required_n"],
        "max_attempted_seed_tuples": cfg["attempt_cap"],
        "stop_rule": "BOTH_ARMS_THEN_REPEAT",
        "repeat_min_executions_per_arm": 2,
        "repeat_attempt_cap": 2,
        "independence_assumption": (
            "Distinct predeclared fuzz seeds are treated as independent only "
            "for the planning calculation; native and Hermit rates are not "
            "asserted equivalent."
        ),
    }
    oracle_id = f"{cfg['name']}-selected-gtest-v1"
    plan = {
        "plan_id": f"{cfg['name']}-kernel71-ptrace-strict-chaos-target-races-v1",
        "plan_sha256": "",
        "sealed_at": SEALED_AT,
        "sealed_by": "codex-tfa-kernel71-campaign",
        "campaign_owner_identity": "rrnewton",
        "authorized_routes": [route],
        "target_oracle": {
            "oracle_id": oracle_id,
            "defined_before_runs": True,
            "unmodified": True,
            "selected_test_executed_predicate": (
                f"GoogleTest output contains [ RUN      ] {cfg['gtest_display']} "
                "and the summary reports exactly one selected test."
            ),
            "pass_predicate": (
                f"The selected test line is [       OK ] {cfg['gtest_display']}, "
                "the summary reports one passed test, and the guest exits 0 "
                "without a signal."
            ),
            "fail_classes": [
                {
                    "failure_class_id": "gtest-selected-test-failure",
                    "target_owned": True,
                    "predicate": (
                        f"The selected test line is [  FAILED  ] {cfg['gtest_display']}, "
                        "the summary reports one failed selected test, and the guest "
                        "exits nonzero, excluding Hermit, wrapper, dependency, "
                        "infrastructure, and outer-timeout failures."
                    ),
                },
                {
                    "failure_class_id": "selected-test-sanitizer-failure",
                    "target_owned": True,
                    "predicate": (
                        f"After [ RUN      ] {cfg['gtest_display']}, the unchanged "
                        "ASan/TSan oracle reports a sanitizer finding causally owned "
                        "by that selected test and the guest exits nonzero, excluding "
                        "Hermit, wrapper, dependency, and infrastructure failures."
                    ),
                },
            ],
            "timeout_policy": {
                "target_timeout_can_fail": False,
                "target_timeout_predicate": None,
                "target_timeout_failure_class_id": None,
                "outer_timeout_is_censored": True,
            },
        },
        "measurement_design": design,
        "resource_bounds": bounds,
        "artifact_policy": {
            "scratch_root": "/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign",
            "terminal_evidence_must_be_durable": True,
            "digest_algorithm": "sha256",
        },
    }
    plan["plan_sha256"] = digest(plan, "plan_sha256")
    return {
        "schema_version": 2,
        "record_type": "TFA_TARGET_EXPERIMENT",
        "contract": {
            "commit": "9d7f603c43618715b9245c74acb3e164c17d2ae8",
            "path": "ai_docs/tfa-acceptance-bracket-2026-09-17.md",
            "blob": "4a53f0c25623ad6371ee86b4556adada1218745d",
        },
        "identity": identity,
        "candidate_ranking": copy.deepcopy(entry["candidate_ranking"]),
        "plan": plan,
        "plan_receipt": None,
        "attempt_state": "PLANNED",
        "cells": [],
        "events": [],
        "samples": [],
        "matchers": [],
        "outcome_history": [],
        "current_outcome": None,
    }


def main() -> None:
    cohort = json.loads((ROOT / "cohort.json").read_text())
    entries = {item["canonical_target"]: item for item in cohort["targets"]}
    for cfg in CONFIGS:
        record = make_plan(entries[cfg["target"]], cfg)
        path = ROOT / record["identity"]["target_directory"] / "experiment.json"
        if path.exists():
            print(f"skip existing {path.relative_to(ROOT)}")
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n")
        print(path.relative_to(ROOT))


if __name__ == "__main__":
    main()
