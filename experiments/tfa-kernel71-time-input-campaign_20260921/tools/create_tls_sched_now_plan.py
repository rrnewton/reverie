#!/usr/bin/env python3
"""Create the prospective TLS terminator scheduler/time-input schema-v2 plan."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TARGET = "fbcode//proxygen/facebook/fwdproxy/tlsterm/test:tls_terminator_test"
SOURCE_REVISION = "7f239d5b37dfbecf32fed144468da461f998add5"
HERMIT_REVISION = "4630ee2a61f93221a43e2baec0a15f34102a2c61"
HERMIT_BINARY = "/home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a"
HERMIT_SHA256 = "5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a"
TEST_BINARY = "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/49bed21049d8ec65/proxygen/facebook/fwdproxy/tlsterm/test/__tls_terminator_test__/tls_terminator_test"
TEST_SHA256 = "e7a5278312bb9daa6d38a9a03cc2001cd31f8c64a92791d92d5aa47c20f66566"
TEST_BYTES = 991_876_712
TEST_DISPLAY = "TlsTerminatorTest/TlsTerminatorTest.terminateTls/h1_innerTls_http_connect"
SEALED_AT = "2026-09-21T18:54:35Z"
EPOCH = "2026-09-21T18:54:35Z"
SEEDS = list(range(16))
VERIFY_DIR = "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-tls-sched-now-2936582-20260921-1036584-1790016867759891372"
BUCK_OUT = "/home/newton/work/tfa/fbsource/buck-out"
CAMPAIGN_CWD = "/home/newton/work/dev-hermit/worktrees/slots/hermit-tfa-kernel71-campaign/experiments/tfa-kernel71-time-input-campaign_20260921"
SCRATCH_ROOT = "/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign/tls-sched-now-input"
HELPER_REL_PATHS = tuple(
    sorted(
        (
            "experiments/tfa-kernel71-time-input-campaign_20260921/tools/"
            "create_tls_sched_now_plan.py",
            "experiments/tfa-kernel71-time-input-campaign_20260921/tools/"
            "run_tls_sched_now_seed.py",
            "experiments/tfa-kernel71-time-input-campaign_20260921/tools/"
            "admit_tls_sched_now_seed.py",
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
    identity["experiment_id"] = "proxygen/tls-terminator-test--60719afd988e"
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
        "stderr_bytes": 67_108_864,
        "stdout_bytes": 67_108_864,
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
    cell_id = "tls-kernel71-ptrace-strict-sched-now-v1"
    route_id = "tls-kernel71-sched-now-existing-elf-v1"
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
        "--base-env=minimal",
        "--env",
        "LC_ALL=C",
        "--env",
        "TZ=UTC",
        f"--mount=type=bind,source={BUCK_OUT},target={BUCK_OUT}",
        f"--epoch={EPOCH}",
        "--clock-multiplier=1.0",
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
        "--max-timeslice",
        "200000000",
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
            f"Processor; PMU type 4/RCB available; explicit realistic epoch {EPOCH}; "
            "clock multiplier 1.0; no chaos-target-races, fuzz-futex, per-thread "
            "slowdown, or happens-before flags; target-timeslice unset; max-timeslice "
            "200000000 virtual ns; only sched-seed varies over 0..15; seed/rng/fuzz "
            "roots 0; "
            "stdout is drained in constant memory, retained to 67108864 bytes, and "
            "the safehermit cgroup is SIGKILLed on first overflow; an overflow is "
            "always censored; "
            f"BitwiseInfoV1 with no relaxations; verifier qgroup {VERIFY_DIR}; host-only "
            "SAFEHERMIT_LOG_ROOT=/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit."
        ),
    }
    route["route_sha256"] = digest(route, "route_sha256")

    design = {
        "purpose": "EXISTENCE_DISCOVERY",
        "swept_coordinate": {
            "flag": "--sched-seed",
            "domain": "finite explicit scheduler seeds 0 through 15 inclusive",
            "scheduler_modes": [],
        },
        "fixed_seed_roots": fixed_roots,
        "seed_population": {"kind": "EXPLICIT_VALUES", "values": SEEDS},
        "selection_order": {"kind": "EXPLICIT", "seed_values": SEEDS},
        "without_replacement": True,
        "design_basis": {
            "kind": "FINITE_POPULATION",
            "population_definition": (
                "Sixteen ascending scheduler seeds fixed prospectively before any run; "
                "no observed TLS outcome influenced the population or order."
            ),
            "population_size": len(SEEDS),
            "selection_rule": (
                "Attempt the explicit order without replacement; stop only after distinct "
                "seeds supply both repeated target arms, otherwise exhaust all sixteen."
            ),
        },
        "miss_probability_override": None,
        "required_valid_seed_tuples": len(SEEDS),
        "max_attempted_seed_tuples": len(SEEDS),
        "stop_rule": "BOTH_ARMS_THEN_REPEAT",
        "repeat_min_executions_per_arm": 2,
        "repeat_attempt_cap": 2,
        "independence_assumption": (
            "No probabilistic independence claim is used; this is a finite, explicit, "
            "source-derived scheduler-seed population."
        ),
    }
    oracle_id = "tls-selected-gtest-and-uncaught-exception-v2"
    plan = {
        "plan_id": "tls-kernel71-ptrace-strict-sched-now-v1",
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
                "must additionally satisfy its class-specific selected assertion or "
                "TestBody-lambda source evidence."
            ),
            "pass_predicate": (
                f"stdout contains exactly one [       OK ] {TEST_DISPLAY}, reports "
                "[  PASSED  ] 1 test., and the guest exits 0 without a signal."
            ),
            "fail_classes": [
                {
                    "failure_class_id": "tls-selected-gtest-failure",
                    "target_owned": True,
                    "predicate": (
                        f"After exactly one [ RUN      ] {TEST_DISPLAY}, stdout contains "
                        "exactly one matching [  FAILED  ] selected-test line and the "
                        "one-failed-test summary; stdout or stderr contains a GoogleTest Failure "
                        "block at TlsTerminatorTest.cpp line 153, 178, 179, 197, 226, "
                        "227, 228, 229, 232, 233, 234, 242, 245, 248, or 267; the guest "
                        "exits 1 without a signal; exclude certificate verification/time, "
                        "Hermit, wrapper, dependency, sanitizer, "
                        "infrastructure, OOM, truncation, and outer-timeout failures."
                    ),
                },
                {
                    "failure_class_id": "tls-selected-uncaught-async-socket-abort",
                    "target_owned": True,
                    "predicate": (
                        f"After exactly one [ RUN      ] {TEST_DISPLAY} and before any "
                        "selected [ OK ] line, stderr contains the exact uncaught "
                        "folly::AsyncSocketException termination marker and a selected "
                        "TlsTerminatorTest_terminateTls_Test::TestBody lambda frame paired "
                        "with TlsTerminatorTest.cpp:308 or :316; the guest terminates with "
                        "the exact stderr Signal 6 (SIGABRT) marker and safehermit reports "
                        "unit_result=core-dump, process_signal=6, exit_code=134. Explicitly exclude "
                        "certificate verification, "
                        "not-yet-valid, expired-certificate, Hermit, wrapper, dependency, "
                        "sanitizer, infrastructure, OOM, truncation, and outer-timeout "
                        "failures."
                    ),
                },
                {
                    "failure_class_id": "tls-selected-uncaught-future-timeout-abort",
                    "target_owned": True,
                    "predicate": (
                        f"After exactly one [ RUN      ] {TEST_DISPLAY} and before any "
                        "selected [ OK ] line, stderr contains the exact uncaught "
                        "folly::FutureTimeout termination marker and a selected "
                        "TlsTerminatorTest_terminateTls_Test::TestBody lambda frame paired "
                        "with TlsTerminatorTest.cpp:308 or :316; the guest terminates with "
                        "the exact stderr Signal 6 (SIGABRT) marker and safehermit reports "
                        "unit_result=core-dump, process_signal=6, exit_code=134. Explicitly exclude "
                        "certificate verification, "
                        "not-yet-valid, expired-certificate, Hermit, wrapper, dependency, "
                        "sanitizer, infrastructure, OOM, truncation, and outer-timeout "
                        "failures."
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
