#!/usr/bin/env python3
"""Create the prospective AsyncSSL factor-10, 50-ms-timeslice schema-v2 plan."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TARGET = "fbcode//folly/io/async/test:async_ssl_socket_test"
SOURCE_REVISION = "7f239d5b37dfbecf32fed144468da461f998add5"
HERMIT_REVISION = "4630ee2a61f93221a43e2baec0a15f34102a2c61"
HERMIT_BINARY = "/home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a"
HERMIT_SHA256 = "5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a"
TEST_BINARY = "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test"
TEST_SHA256 = "47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46"
TEST_BYTES = 17_651_096
TEST_DISPLAY = "AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect"
SEALED_AT = "2026-09-21T22:29:36Z"
EPOCH = "2026-09-21T22:29:36Z"
SEEDS = list(range(16))
VERIFY_DIR = "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-k71-asyncssl-slice50-2936582-3221751-1790029768678752817"
BUCK_OUT = "/home/newton/work/tfa/fbsource/buck-out"
CAMPAIGN_CWD = "/home/newton/work/dev-hermit/worktrees/slots/hermit-tfa-kernel71-campaign/experiments/tfa-kernel71-asyncssl-slice50-continuation_20260921"
SCRATCH_ROOT = "/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign/asyncssl-sched-slowdown-factor10-slice50m"
HELPER_REL_PATHS = tuple(
    sorted(
        (
            "experiments/tfa-kernel71-asyncssl-slice50-continuation_20260921/tools/create_asyncssl_sched_slowdown_plan.py",
            "experiments/tfa-kernel71-asyncssl-slice50-continuation_20260921/tools/run_asyncssl_sched_slowdown_seed.py",
            "experiments/tfa-kernel71-asyncssl-slice50-continuation_20260921/tools/admit_asyncssl_sched_slowdown_seed.py",
        )
    )
)


def digest(value: dict, key: str) -> str:
    canonical = dict(value)
    canonical.pop(key, None)
    payload = json.dumps(
        canonical, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    return hashlib.sha256(payload).hexdigest()


def main() -> None:
    cohort = json.loads((ROOT / "cohort.json").read_text())
    entry = next(item for item in cohort["targets"] if item["canonical_target"] == TARGET)
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
    identity["experiment_id"] = "folly/async-ssl-socket-test--cefc173a09b4"
    identity["alias_resolution"] = {
        "input_target": entry["invoked_target"],
        "resolved_target": entry["canonical_target"],
        "method": "pinned-cohort-target-and-existing-buck-output",
        "source_revision": SOURCE_REVISION,
        "evidence": (
            f"The fixed cohort resolves this canonical target. Existing executable "
            f"{TEST_BINARY} is {TEST_BYTES} bytes with SHA-256 {TEST_SHA256}."
        ),
    }

    bounds = {
        "wall_seconds": 120,
        "memory_bytes": 17_179_869_184,
        "swap_bytes": 0,
        "stdout_bytes": 67_108_864,
        "stderr_bytes": 67_108_864,
        "retained_log_bytes": 67_108_864,
        "disk_guard": "8G",
    }
    fixed_roots = {
        "--seed": 0,
        "--rng-seed": 0,
        "--sched-seed": None,
        "--fuzz-seed": 0,
    }
    guest = [
        TEST_BINARY,
        f"--gtest_filter={TEST_DISPLAY}",
        "--gtest_also_run_disabled_tests",
    ]
    cell_id = "asyncssl-kernel71-ptrace-strict-chaos-sched-slowdown-factor10-slice50m-now-v1"
    route_id = "asyncssl-kernel71-sched-slowdown-factor10-slice50m-existing-elf-v1"
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
        "--chaos-per-thread-slowdown",
        "--chaos-slowdown-max-factor=10.0",
        "--chaos-epoch-length-ns=0",
        "--base-env=minimal",
        "--env",
        "LC_ALL=C",
        "--env",
        "TZ=UTC",
        f"--mount=type=bind,source={BUCK_OUT},target={BUCK_OUT}",
        f"--epoch={EPOCH}",
        "--seed",
        "0",
        "--rng-seed",
        "0",
        "--sched-seed",
        "{SWEEP}",
        "--fuzz-seed",
        "0",
        "--sched-heuristic",
        "random",
        "--target-timeslice",
        "50000000",
        "--max-timeslice",
        "200000000",
        "--clock-multiplier=1.0",
        "--verify",
        "--verify-strict",
        "--verify-allow",
        "both",
        "--keep-logs",
        f"--verify-log-dir={VERIFY_DIR}",
        "--",
        *guest,
    ]
    route = {
        "route_id": route_id,
        "route_sha256": "",
        "fallback_of": None,
        "authorized_cell_ids": [cell_id],
        "canonical_target": entry["canonical_target"],
        "test_selector": copy.deepcopy(entry["test_selector"]),
        "build_mode_selector": copy.deepcopy(entry["build_mode_selector"]),
        "source_revision": SOURCE_REVISION,
        "build_argv": copy.deepcopy(entry["standalone_argv"]),
        "launch_argv": launch,
        "repeat_matcher": {"kind": "BUILTIN_VERIFY", "comparison": "BitwiseInfoV1"},
        "environment": {"LC_ALL": "C", "TZ": "UTC"},
        "notes": (
            f"PRE-RUN SEAL: one authorized cell {cell_id}; Hermit revision "
            f"{HERMIT_REVISION}, binary SHA-256 {HERMIT_SHA256}; test ELF SHA-256 "
            f"{TEST_SHA256}; host devbig030.atn3.facebook.com, kernel Linux "
            "7.1.3-0_fbk0_0_g295ad0959f34, x86_64 AMD EPYC 9D85 158-Core "
            f"Processor; PMU type 4/RCB available; explicit epoch {EPOCH}; normal "
            "clock progression with explicit multiplier 1.0; per-thread slowdown "
            "enabled with max factor 10.0 and epoch length 0; only --sched-seed "
            "varies over the explicit 0..15 finite population; target-timeslice "
            "50000000 virtual ns; max-timeslice 200000000 virtual ns; seed/rng/fuzz "
            "roots 0; --chaos-target-races absent; "
            f"BitwiseInfoV1 with no relaxations; verifier qgroup {VERIFY_DIR}; host-only "
            "SAFEHERMIT_LOG_ROOT=/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit."
        ),
    }
    route["route_sha256"] = digest(route, "route_sha256")

    design = {
        "purpose": "EXISTENCE_DISCOVERY",
        "swept_coordinate": {
            "flag": "--sched-seed",
            "domain": "finite predeclared values 0 through 15 inclusive",
            "scheduler_modes": [],
        },
        "fixed_seed_roots": fixed_roots,
        "seed_population": {"kind": "EXPLICIT_VALUES", "values": SEEDS},
        "selection_order": {"kind": "EXPLICIT", "seed_values": SEEDS},
        "without_replacement": True,
        "design_basis": {
            "kind": "FINITE_POPULATION",
            "population_definition": (
                "The prospectively enumerated scheduler-seed values 0 through 15 "
                "under the single frozen AsyncSSL factor-10 slowdown cell with a "
                "50000000-virtual-ns target timeslice."
            ),
            "population_size": len(SEEDS),
            "selection_rule": "Ascending numeric order without replacement.",
        },
        "miss_probability_override": None,
        "required_valid_seed_tuples": len(SEEDS),
        "max_attempted_seed_tuples": len(SEEDS),
        "stop_rule": "BOTH_ARMS_THEN_REPEAT",
        "repeat_min_executions_per_arm": 2,
        "repeat_attempt_cap": 2,
        "independence_assumption": (
            "No probabilistic independence or miss-rate claim is made; this is an "
            "exhaustive traversal of the explicitly bounded 16-value population."
        ),
    }
    oracle_id = "asyncssl-selected-gtest-v2"
    plan = {
        "plan_id": "asyncssl-kernel71-ptrace-strict-sched-slowdown-factor10-slice50m-now-v1",
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
                f"stdout contains exactly one [ RUN      ] {TEST_DISPLAY}; the exact "
                "gtest filter selects only this case, and any abnormal target verdict "
                "must occur in that selected-test output context. The disabled-test flag "
                "is explicit because the test fixture is disabled by default."
            ),
            "pass_predicate": (
                f"stdout contains exactly one [       OK ] {TEST_DISPLAY}, reports "
                "[  PASSED  ] 1 test., both exact source-pinned failure blocks are absent, "
                "and the guest exits 0 without a signal or sanitizer failure."
            ),
            "fail_classes": [
                {
                    "failure_class_id": "gtest-byte-events-three-missing-ack",
                    "target_owned": True,
                    "predicate": (
                        f"After exactly one [ RUN      ] {TEST_DISPLAY} and before any "
                        "selected [ OK ] line, stdout contains exactly one source-pinned "
                        "fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp:3797: Failure "
                        "block stating observer->byteEvents expected size >= 4 but actual "
                        "size 3, followed by exactly one source-pinned line-3812 equality "
                        "block stating offsetExpected is 501 but the ACK max-offset value "
                        "is the all-zero 16-byte object. The selected test then has its "
                        "timed [  FAILED  ] line and one-failed-test summary, and the guest "
                        "exits with code 1. Either block alone, a different block value or "
                        "source line, signal-only, sanitizer-only, Hermit, wrapper, loader, "
                        "dependency, infrastructure, OOM, truncation, and outer-timeout "
                        "failures are excluded."
                    ),
                }
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
            "scratch_root": SCRATCH_ROOT,
            "terminal_evidence_must_be_durable": True,
            "digest_algorithm": "sha256",
        },
    }
    plan["plan_sha256"] = digest(plan, "plan_sha256")
    record = {
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
    path = ROOT / entry["target_directory"] / "experiment.json"
    if path.exists():
        raise SystemExit(f"refusing to overwrite existing plan: {path}")
    path.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n")
    print(path.relative_to(ROOT))


if __name__ == "__main__":
    main()
