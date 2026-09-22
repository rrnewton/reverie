#!/usr/bin/env python3
"""Create the prospective HTTPCoro scheduler/time-input schema-v2 plan."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TARGET = "fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests"
SOURCE_REVISION = "7f239d5b37dfbecf32fed144468da461f998add5"
HERMIT_REVISION = "4630ee2a61f93221a43e2baec0a15f34102a2c61"
HERMIT_BINARY = "/home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a"
HERMIT_SHA256 = "5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a"
TEST_BINARY = "/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests"
TEST_SHA256 = "00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc"
TEST_BYTES = 48_908_168
TEST_DISPLAY = "HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2"
SEALED_AT = "2026-09-21T18:14:41Z"
EPOCH = "2026-09-21T18:14:41Z"
SEEDS = [7, 5, 4, 2, 15, 0, 3, 1]
VERIFY_DIR = "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-httpcoro-time-2936582-20260921-2670399-1790014467662735185"
BUCK_OUT = "/home/newton/work/tfa/fbsource/buck-out"
CAMPAIGN_CWD = "/home/newton/work/dev-hermit/worktrees/slots/hermit-tfa-kernel71-campaign/experiments/tfa-kernel71-time-input-campaign_20260921"
SCRATCH_ROOT = "/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign/httpcoro-time-input"


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
    identity["experiment_id"] = "proxygen/httpcorosessiontests--e3f69b337304"
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
        "retained_log_bytes": 67_108_864,
        "disk_guard": "8G",
    }
    fixed_roots = {
        "--seed": 0,
        "--rng-seed": 0,
        "--sched-seed": None,
        "--fuzz-seed": 0,
    }
    guest = [TEST_BINARY, f"--gtest_filter={TEST_DISPLAY}"]
    cell_id = "httpcoro-kernel71-ptrace-strict-sched-slowdown-v1"
    route_id = "httpcoro-kernel71-time-input-existing-elf-v1"
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
        "--clock-multiplier=0.125",
        "--chaos-per-thread-slowdown",
        "--chaos-slowdown-max-factor=10.0",
        "--chaos-epoch-length-ns=0",
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
            f"Processor; PMU type 4/RCB available; explicit epoch {EPOCH}; fixed "
            "clock multiplier 0.125; constant deterministic per-thread slowdown "
            "factor selected only by the swept scheduler seed; target-timeslice "
            "unset; max-timeslice 200000000 virtual ns; seed/rng/fuzz roots 0; "
            f"BitwiseInfoV1 with no relaxations; verifier qgroup {VERIFY_DIR}; host-only "
            "SAFEHERMIT_LOG_ROOT=/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit."
        ),
    }
    route["route_sha256"] = digest(route, "route_sha256")

    design = {
        "purpose": "EXISTENCE_DISCOVERY",
        "swept_coordinate": {
            "flag": "--sched-seed",
            "domain": (
                "finite explicit scheduler seeds [7,5,4,2,15,0,3,1], selected "
                "before execution from source-derived dtid-3 slowdown factors"
            ),
            "scheduler_modes": [],
        },
        "fixed_seed_roots": fixed_roots,
        "seed_population": {"kind": "EXPLICIT_VALUES", "values": SEEDS},
        "selection_order": {"kind": "EXPLICIT", "seed_values": SEEDS},
        "without_replacement": True,
        "design_basis": {
            "kind": "FINITE_POPULATION",
            "population_definition": (
                "Eight scheduler seeds fixed before execution to alternate low and high "
                "Q32 per-thread virtual-time factors for deterministic thread 3."
            ),
            "population_size": len(SEEDS),
            "selection_rule": (
                "Attempt the explicit order without replacement; stop only after distinct "
                "seeds supply both repeated target arms, otherwise exhaust all eight."
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
    oracle_id = "httpcoro-reset-rate-limit-exact-v2"
    plan = {
        "plan_id": "httpcoro-kernel71-ptrace-strict-sched-time-v1",
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
                "must additionally carry its selected-test stack frame."
            ),
            "pass_predicate": (
                f"stdout contains exactly one [       OK ] {TEST_DISPLAY}, reports "
                "[  PASSED  ] 1 test., and the guest exits 0 without a signal."
            ),
            "fail_classes": [
                {
                    "failure_class_id": "reset-rate-limit-line1685-body-event-fatal",
                    "target_owned": True,
                    "predicate": (
                        f"After exactly one [ RUN      ] {TEST_DISPLAY} and before any "
                        "selected [ OK ] line, stderr contains exactly one "
                        "HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: "
                        "bodyEvent.hasException(), the stack names "
                        "H2DownstreamSessionTest_ResetRateLimit_Test, and the guest "
                        "terminates with SIGABRT/exit 134; exclude Hermit, wrapper, "
                        "dependency, sanitizer, infrastructure, OOM, truncation, and "
                        "outer-timeout failures."
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
