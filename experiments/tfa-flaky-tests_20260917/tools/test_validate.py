#!/usr/bin/env python3
"""Mutation tests for the dependency-free TFA experiment validator."""

from __future__ import annotations

import copy
import hashlib
import json
import signal as signal_module
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parent
sys.path.insert(0, str(TOOLS))
import validate as validator  # noqa: E402

SHA1 = "1" * 40
SHA256 = "2" * 64
WHEN = "2026-09-18T04:00:00Z"
FINISH = "2026-09-18T04:00:01Z"
COMPARATOR_CONTENT = b"#!/usr/bin/env python3\n# synthetic comparator fixture\n"


def artifact(artifact_id: str, kind: str) -> dict:
    return {
        "artifact_id": artifact_id,
        "kind": kind,
        "storage": "TRACKED",
        "locator": f"evidence/{artifact_id}.txt",
        "sha256": SHA256,
        "bytes": 1,
        "capture_limit_bytes": 1024,
        "truncated": False,
        "media_type": "text/plain",
    }


def _json_payload(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def artifact_payloads(record: dict) -> dict[str, bytes]:
    payloads: dict[str, bytes] = {}
    cells = {cell["cell_id"]: cell for cell in record["cells"]}
    for event in record["events"]:
        for item in event["artifacts"]:
            if item["kind"] == "RESULT":
                payloads[item["artifact_id"]] = _json_payload(
                    validator._event_result_payload(event)
                )
            elif item["kind"] == "DIAGNOSTIC":
                payloads[item["artifact_id"]] = (
                    event["result"]["diagnostic"] + "\n"
                ).encode()
            elif item["kind"] == "SAFEHERMIT_REPORT":
                binary_hash = cells[event["cell_id"]]["hermit_binary_sha256"]
                bounds = record["plan"]["resource_bounds"]
                result = event["result"]
                signal_name = result["signal"]
                signal_number = (
                    signal_module.Signals[signal_name].value
                    if signal_name is not None else None
                )
                final_exit = 128 + signal_number if signal_number is not None else result["exit_code"]
                unit_result = "signal" if signal_name is not None else (
                    "success" if final_exit == 0 else "exit-code"
                )
                rows = {
                    "run_id": event["event_id"], "binary": "/tmp/hermit",
                    "binary_source": "explicit", "binary_sha256": binary_hash,
                    "log_dir": "/tmp/safehermit", "log_file": "/tmp/safehermit/stderr.log",
                    "unit": "tfa-fixture.scope",
                    "bound.wall": f"APPLIED:{bounds['wall_seconds']}s",
                    "bound.cgroup": "APPLIED:MemoryMax=16G MemorySwapMax=0",
                    "bound.disk": f"APPLIED:{bounds['disk_guard']} for log_dir only; synthetic fixture",
                    "bound.bytes": f"APPLIED:{bounds['stderr_bytes']} (LETHAL: synthetic fixture)",
                    "bound.logfilter": "NOT_APPLIED:synthetic fixture",
                    "env.forwarded": "synthetic fixture",
                    "note.run_record": "synthetic fixture",
                    "elapsed_secs": "1", "bytes_written": "1", "truncated": "false",
                    "runner_exit_code": str(final_exit), "unit_result": unit_result,
                    "exit_code": str(final_exit),
                }
                if signal_number is not None:
                    rows["process_signal"] = str(signal_number)
                payloads[item["artifact_id"]] = "".join(
                    f"safehermit: {key}={value}\n" for key, value in rows.items()
                ).encode()
    for route in record["plan"]["authorized_routes"]:
        matcher_plan = route["repeat_matcher"]
        if matcher_plan["kind"] == "EQUIVALENT":
            item = matcher_plan["comparator_artifact"]
            payloads[item["artifact_id"]] = COMPARATOR_CONTENT
    for matcher in record["matchers"]:
        item = matcher["canonical_info_artifact"]
        payloads[item["artifact_id"]] = _json_payload(
            validator._matcher_info_payload(matcher)
        )
    current = record["current_outcome"]
    if isinstance(current, dict) and current.get("kind") == "BLOCKED-WITH-NAMED-CAUSE":
        adjudication = current["cause"]["campaign_owner_adjudication"]
        artifact_id = adjudication["evidence_artifact_id"]
        payloads[artifact_id] = _json_payload({
            "schema_version": 1,
            **{key: value for key, value in adjudication.items() if key != "evidence_artifact_id"},
        })
    return payloads


def _identity(entry: dict) -> dict:
    keys = (
        "service", "canonical_target", "target_key_sha256", "target_directory",
        "testinfra_id", "test_name", "source_label_qualifier", "test_selector",
        "build_mode_selector", "invoked_target", "standalone_argv",
    )
    directory = entry["target_directory"].rsplit("/", 1)[-1]
    result = {key: copy.deepcopy(entry[key]) for key in keys}
    result["experiment_id"] = entry["service"] + "/" + directory
    result["alias_resolution"] = {
        "input_target": entry["canonical_target"],
        "resolved_target": entry["canonical_target"],
        "method": "pinned-source-resolution",
        "source_revision": SHA1,
        "evidence": "synthetic unit fixture",
    }
    return result


def _minimum_n(p: float, miss: float = 0.05) -> int:
    n = 2
    while p ** n + (1 - p) ** n > miss:
        n += 1
    return n


def planned_record(entry: dict, matcher_kind: str = "BUILTIN_VERIFY") -> dict:
    identity = _identity(entry)
    n = _minimum_n(0.25)
    seeds = [7, 19] + [value for value in range(256) if value not in {7, 19}]
    launch = [
        "bin/safehermit", "--sh-report", "{SAFEHERMIT_REPORT}",
        "--sh-deadline", "60", "--sh-max-log-bytes", "1024",
        "--sh-disk-limit", "8G", "run",
        "--backend", "ptrace", "--strict", "--chaos",
        "--seed", "0", "--rng-seed", "0", "--fuzz-seed", "0",
        "--sched-seed", "{SWEEP}", "--sched-heuristic", "random",
        "--max-timeslice", "200000000",
    ]
    if matcher_kind == "BUILTIN_VERIFY":
        launch.extend(["--verify", "--verify-strict", "--verify-allow", "both"])
        repeat_matcher = {"kind": "BUILTIN_VERIFY", "comparison": "BitwiseInfoV1"}
    else:
        comparator = artifact("sealed-comparator", "OTHER")
        comparator.update(
            locator=entry["target_directory"] + "/sealed-comparator.py",
            sha256=hashlib.sha256(COMPARATOR_CONTENT).hexdigest(),
            bytes=len(COMPARATOR_CONTENT), media_type="text/x-python",
        )
        repeat_matcher = {
            "kind": "EQUIVALENT",
            "comparator_argv": ["python3", comparator["locator"]],
            "comparator_artifact": comparator,
        }
    launch.extend(["--", "/tmp/test"])
    route = {
        "route_id": "route-primary",
        "route_sha256": "0" * 64,
        "fallback_of": None,
        "authorized_cell_ids": ["cell-1"],
        "canonical_target": entry["canonical_target"],
        "test_selector": copy.deepcopy(entry["test_selector"]),
        "build_mode_selector": copy.deepcopy(entry["build_mode_selector"]),
        "source_revision": SHA1,
        "build_argv": copy.deepcopy(entry["standalone_argv"]),
        "launch_argv": launch,
        "repeat_matcher": repeat_matcher,
        "environment": {"LC_ALL": "C"},
        "notes": "synthetic test route",
    }
    route["route_sha256"] = validator.canonical_object_sha256(route, "route_sha256")
    plan = {
        "plan_id": "plan-1",
        "plan_sha256": "0" * 64,
        "sealed_at": WHEN,
        "sealed_by": "synthetic-unit-test",
        "campaign_owner_identity": "rrnewton",
        "authorized_routes": [route],
        "target_oracle": {
            "oracle_id": "oracle-1",
            "defined_before_runs": True,
            "unmodified": True,
            "selected_test_executed_predicate": "one selected case ran",
            "pass_predicate": "selected case passed",
            "fail_classes": [{"failure_class_id": "assertion-1", "target_owned": True, "predicate": "selected assertion failed"}],
            "timeout_policy": {"target_timeout_can_fail": False, "target_timeout_predicate": None, "target_timeout_failure_class_id": None, "outer_timeout_is_censored": True},
        },
        "measurement_design": {
            "purpose": "EXISTENCE_DISCOVERY",
            "swept_coordinate": {"flag": "--sched-seed", "domain": "explicit set", "scheduler_modes": []},
            "fixed_seed_roots": {"--seed": 0, "--rng-seed": 0, "--sched-seed": None, "--fuzz-seed": 0},
            "seed_population": {"kind": "EXPLICIT_VALUES", "values": seeds},
            "selection_order": {"kind": "EXPLICIT", "seed_values": seeds},
            "without_replacement": True,
            "design_basis": {
                "kind": "RATE_BOUND",
                "expected_hermit_fail_probability": 0.25,
                "provenance_kind": "MINIMUM_DETECTABLE_RATE",
                "provenance": "synthetic minimum detectable rate",
                "equivalence_to_hermit": False,
                "max_miss_probability": 0.05,
            },
            "miss_probability_override": None,
            "required_valid_seed_tuples": n,
            "max_attempted_seed_tuples": n + 4,
            "stop_rule": "BOTH_ARMS_THEN_REPEAT",
            "repeat_min_executions_per_arm": 2,
            "repeat_attempt_cap": 4,
            "independence_assumption": "synthetic independent-seed assumption",
        },
        "resource_bounds": {"wall_seconds": 60, "memory_bytes": 17179869184, "swap_bytes": 0, "stderr_bytes": 1024, "retained_log_bytes": 4096, "disk_guard": "8G"},
        "artifact_policy": {"scratch_root": "ignored/tfa", "terminal_evidence_must_be_durable": True, "digest_algorithm": "sha256"},
    }
    plan["plan_sha256"] = validator.canonical_object_sha256(plan, "plan_sha256")
    return {
        "schema_version": 2,
        "record_type": "TFA_TARGET_EXPERIMENT",
        "contract": {"commit": "9d7f603c43618715b9245c74acb3e164c17d2ae8", "path": "ai_docs/tfa-acceptance-bracket-2026-09-17.md", "blob": "4a53f0c25623ad6371ee86b4556adada1218745d"},
        "identity": identity,
        "candidate_ranking": copy.deepcopy(entry["candidate_ranking"]),
        "plan": plan,
        "plan_receipt": None,
        "attempt_state": "PLANNED",
        "cells": [], "events": [], "samples": [], "matchers": [],
        "outcome_history": [], "current_outcome": None,
    }


def working_record(entry: dict, matcher_kind: str = "BUILTIN_VERIFY") -> dict:
    record = planned_record(entry, matcher_kind)
    plan = record["plan"]
    route = plan["authorized_routes"][0]
    cell = {
        "cell_id": "cell-1",
        "route_id": route["route_id"],
        "route_sha256": route["route_sha256"],
        "plan_sha256": plan["plan_sha256"],
        "frozen_at": WHEN,
        "cell_sha256": "0" * 64,
        "canonical_target": record["identity"]["canonical_target"],
        "test_selector": copy.deepcopy(record["identity"]["test_selector"]),
        "build_mode_selector": copy.deepcopy(record["identity"]["build_mode_selector"]),
        "target_source_revision": SHA1,
        "hermit_source_revision": SHA1,
        "hermit_binary_sha256": SHA256,
        "test_artifact_sha256": SHA256,
        "guest": {"argv": ["/tmp/test"], "environment": {"LC_ALL": "C"}, "mounts": [], "immutable_inputs": [], "working_directory": "/tmp"},
        "hermit": {
            "backend": "ptrace", "strict": True, "chaos": True, "relaxations": [],
            "argv": copy.deepcopy(route["launch_argv"]),
            "scheduler_heuristic": "random",
            "swept_coordinate": "--sched-seed",
            "fixed_seed_roots": copy.deepcopy(plan["measurement_design"]["fixed_seed_roots"]),
        },
        "timing": {"requested_max_timeslice": 200000000, "effective_max_timeslice": 200000000},
        "pmu": {"available": True, "effective_timer": "RCB", "fallback_recorded": False},
        "host": {"hostname": "synthetic", "kernel": "synthetic", "architecture": "x86_64", "cpu": "synthetic", "capabilities": {"pmu": True}},
        "oracle_id": "oracle-1",
        "resource_bounds": copy.deepcopy(plan["resource_bounds"]),
    }
    cell["cell_sha256"] = validator.canonical_object_sha256(cell, "cell_sha256")
    events, samples, matchers = [], [], []
    for selection_index, (outcome, seed) in enumerate((("PASS", 7), ("FAIL", 19))):
        event_ids = []
        for replica in (1, 2):
            event_id = f"event-{outcome.lower()}-{replica}"
            event_ids.append(event_id)
            event_artifacts = [
                artifact(f"{event_id}-stdout", "STDOUT"),
                artifact(f"{event_id}-stderr", "STDERR"),
                artifact(f"{event_id}-result", "RESULT"),
                artifact(f"{event_id}-safehermit", "SAFEHERMIT_REPORT"),
            ]
            argv = validator._replace_flag_value(route["launch_argv"], "--sched-seed", str(seed))
            argv = [event_artifacts[3]["locator"] if token == "{SAFEHERMIT_REPORT}" else token for token in argv]
            events.append({
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
                "started_at": WHEN,
                "finished_at": FINISH,
                "argv": argv,
                "result": {
                    "exit_code": 0 if outcome == "PASS" else 1,
                    "signal": None,
                    "timed_out": False,
                    "timeout_origin": "NONE",
                    "guest_started": True,
                    "selected_test_executed_count": 1,
                    "target_oracle": outcome,
                    "failure_class_id": None if outcome == "PASS" else "assertion-1",
                    "harness_status": "COMPLETE",
                    "diagnostic": "synthetic",
                },
                "invocation_id": f"verify-{outcome.lower()}",
                "replica_index": replica,
                "seed": {"flag": "--sched-seed", "value": seed},
                "effective_config_sha256": cell["cell_sha256"],
                "artifacts": event_artifacts,
            })
        sample_id = f"sample-{outcome.lower()}"
        matcher_id = f"matcher-{outcome.lower()}"
        samples.append({
            "sample_id": sample_id,
            "cell_id": cell["cell_id"],
            "seed": {"flag": "--sched-seed", "value": seed},
            "selection_index": selection_index,
            "status": "VALID",
            "target_outcome": outcome,
            "failure_class_id": None if outcome == "PASS" else "assertion-1",
            "censor_reason": None,
            "execution_event_ids": list(event_ids),
        })
        if matcher_kind == "BUILTIN_VERIFY":
            details = {
                "verified": True, "bitwise_parity": True,
                "comparison": "BitwiseInfoV1", "relaxations": [],
            }
        else:
            sealed = route["repeat_matcher"]
            details = {
                "comparator_argv": copy.deepcopy(sealed["comparator_argv"]),
                "comparator_sha256": sealed["comparator_artifact"]["sha256"],
                "exit_signal_match": True, "stdout_match": True,
                "stderr_match": True, "canonical_info_match": True,
                "relaxations": [],
            }
        matchers.append({
            "matcher_id": matcher_id,
            "kind": matcher_kind,
            "execution_event_ids": list(event_ids),
            "complete": True,
            "same_target_outcome": True,
            "same_failure_class": True,
            "canonical_info_artifact": artifact(f"{matcher_id}-info", "CANONICAL_INFO"),
            "details": details,
        })
    outcome = {
        "classification_id": "classification-working",
        "classified_at": "2026-09-18T04:01:00Z",
        "supersedes": None,
        "evidence_refs": ["sample-pass", "matcher-pass", "sample-fail", "matcher-fail"],
        "kind": "WORKING",
        "pass_witness": {"sample_id": "sample-pass", "matcher_id": "matcher-pass"},
        "fail_witness": {"sample_id": "sample-fail", "matcher_id": "matcher-fail"},
    }
    record.update(attempt_state="MATERIAL", cells=[cell], events=events, samples=samples, matchers=matchers, outcome_history=[outcome], current_outcome=copy.deepcopy(outcome))
    return record


def preguest_event(record: dict, event_id: str = "event-build-refusal") -> dict:
    route = record["plan"]["authorized_routes"][0]
    return {
        "event_id": event_id,
        "kind": "BUILD",
        "plan_sha256": record["plan"]["plan_sha256"],
        "route_id": route["route_id"],
        "route_sha256": route["route_sha256"],
        "canonical_target": route["canonical_target"],
        "test_selector": copy.deepcopy(route["test_selector"]),
        "build_mode_selector": copy.deepcopy(route["build_mode_selector"]),
        "source_revision": route["source_revision"],
        "cell_id": None,
        "cell_sha256": None,
        "measurement_role": None,
        "started_at": WHEN,
        "finished_at": FINISH,
        "argv": copy.deepcopy(route["build_argv"]),
        "result": {
            "exit_code": 1, "signal": None, "timed_out": False,
            "timeout_origin": "NONE", "guest_started": False,
            "selected_test_executed_count": 0, "target_oracle": "NOT_RUN",
            "failure_class_id": None, "harness_status": "ERROR",
            "diagnostic": "ENOTSUP: synthetic unsupported operation",
        },
        "invocation_id": None, "replica_index": None, "seed": None,
        "effective_config_sha256": None,
        "artifacts": [
            artifact(f"{event_id}-diagnostic", "DIAGNOSTIC"),
            artifact(f"{event_id}-result", "RESULT"),
        ],
    }


def blocked_record(entry: dict) -> dict:
    record = planned_record(entry)
    event = preguest_event(record)
    adjudication_artifact = artifact("campaign-owner-adjudication", "ADJUDICATION")
    event["artifacts"].append(adjudication_artifact)
    outcome = {
        "classification_id": "classification-blocked",
        "classified_at": "2026-09-18T04:01:00Z",
        "supersedes": None,
        "evidence_refs": [event["event_id"], adjudication_artifact["artifact_id"]],
        "kind": "BLOCKED-WITH-NAMED-CAUSE",
        "cause": {
            "cause_id": "cause-unsupported",
            "component": "target build",
            "stage": "BUILD",
            "diagnostic": {"kind": "UNSUPPORTED", "code": "ENOTSUP", "message": "ENOTSUP: synthetic unsupported operation"},
            "causal_argument": "sealed route reaches this boundary",
            "route_evidence": [{"route_id": "route-primary", "cell_ids": ["cell-1"], "evidence_event_ids": [event["event_id"]]}],
            "campaign_owner_adjudication": {
                "owner_identity": "rrnewton",
                "decided_at": "2026-09-18T04:00:30Z",
                "authority": "CAMPAIGN_OWNER",
                "disposition": "OUTSIDE_SCOPE",
                "rationale": "synthetic decision",
                "evidence_artifact_id": adjudication_artifact["artifact_id"],
            },
        },
    }
    record.update(attempt_state="MATERIAL", events=[event], outcome_history=[outcome], current_outcome=copy.deepcopy(outcome))
    return record


def unknown_record(entry: dict) -> dict:
    record = planned_record(entry)
    event = preguest_event(record, "event-inspection")
    outcome = {
        "classification_id": "classification-unknown",
        "classified_at": "2026-09-18T04:01:00Z",
        "supersedes": None,
        "evidence_refs": [event["event_id"]],
        "kind": "NOT-DETERMINABLE",
        "route_evidence": [{"route_id": "route-primary", "cell_ids": ["cell-1"], "evidence_event_ids": [event["event_id"]]}],
        "inspected_commands": [copy.deepcopy(event["argv"])],
        "inspected_artifacts": [event["artifacts"][0]["artifact_id"]],
        "missing_or_ambiguous_facts": ["no guest execution"],
        "measurement_budget_exhausted": False,
        "pending_in_scope_measurements": ["instantiate and run cell"],
    }
    record.update(attempt_state="MATERIAL", events=[event], outcome_history=[outcome], current_outcome=copy.deepcopy(outcome))
    return record


class ValidatorTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="tfa-validator-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        shutil.copy2(ROOT / "cohort.json", self.root / "cohort.json")
        shutil.copy2(ROOT / "experiment.schema.json", self.root / "experiment.schema.json")
        shutil.copy2(ROOT / "metadata.json", self.root / "metadata.json")
        for service in validator.ALLOWED_SERVICES:
            (self.root / "services" / service).mkdir(parents=True)
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(
            ["git", "add", "--", "cohort.json", "experiment.schema.json", "metadata.json"],
            cwd=self.root, check=True,
        )
        subprocess.run(
            ["git", "-c", "user.name=TFA validator test",
             "-c", "user.email=tfa-validator@example.invalid", "commit", "-q",
             "-m", "fixture campaign authorities"], cwd=self.root, check=True,
        )
        self.cohort = json.loads((self.root / "cohort.json").read_text())
        self.entry = self.cohort["targets"][0]

    def namespace_artifacts(self, record: dict) -> None:
        prefix = record["identity"]["target_directory"] + "/"
        replacements: dict[str, str] = {}
        for item in validator._artifact_objects(record):
            locator = item["locator"]
            if item["storage"] == "TRACKED" and not locator.startswith(prefix):
                replacements[locator] = prefix + locator
                item["locator"] = prefix + locator
        for event in record["events"]:
            event["argv"] = [replacements.get(token, token) for token in event["argv"]]
        outcomes = list(record["outcome_history"])
        if isinstance(record["current_outcome"], dict):
            outcomes.append(record["current_outcome"])
        for outcome in outcomes:
            if "inspected_commands" in outcome:
                outcome["inspected_commands"] = [
                    [replacements.get(token, token) for token in command]
                    for command in outcome["inspected_commands"]
                ]

    def write_record(self, record: dict) -> Path:
        self.namespace_artifacts(record)
        path = self.root / record["identity"]["target_directory"] / "experiment.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        payloads = artifact_payloads(record)
        plan_locators: list[str] = []
        for item in validator._artifact_objects(record["plan"]):
            if item["storage"] != "TRACKED":
                continue
            content = payloads[item["artifact_id"]]
            if len(content) != item["bytes"] or hashlib.sha256(content).hexdigest() != item["sha256"]:
                raise AssertionError("sealed comparator fixture metadata disagrees")
            artifact_path = self.root / item["locator"]
            artifact_path.parent.mkdir(parents=True, exist_ok=True)
            artifact_path.write_bytes(content)
            subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
            plan_locators.append(item["locator"])
        if record["attempt_state"] == "MATERIAL":
            planned = copy.deepcopy(record)
            planned.update(
                plan_receipt=None, attempt_state="PLANNED", cells=[], events=[],
                samples=[], matchers=[], outcome_history=[], current_outcome=None,
            )
            path.write_text(json.dumps(planned, indent=2, sort_keys=True) + "\n")
            self.commit_record(path, plan_locators)
            plan_commit = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=self.root, check=True,
                capture_output=True, text=True,
            ).stdout.strip()
            relative = path.relative_to(self.root).as_posix()
            plan_blob = subprocess.run(
                ["git", "rev-parse", f"HEAD:{relative}"], cwd=self.root,
                check=True, capture_output=True, text=True,
            ).stdout.strip()
            record["plan_receipt"] = {
                "commit": plan_commit, "path": relative, "blob": plan_blob,
            }
        tracked_locators: list[str] = []
        for item in validator._artifact_objects(record):
            if item["storage"] != "TRACKED":
                continue
            tracked_locators.append(item["locator"])
            artifact_path = self.root / item["locator"]
            artifact_path.parent.mkdir(parents=True, exist_ok=True)
            content = payloads.get(item["artifact_id"], (item["artifact_id"] + "\n").encode())
            artifact_path.write_bytes(content)
            item["bytes"] = len(content)
            item["sha256"] = hashlib.sha256(content).hexdigest()
            subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
        path.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
        self.commit_record(path, sorted(set(tracked_locators + plan_locators)))
        return path

    def commit_record(self, path: Path, extra_paths: list[str] | None = None) -> None:
        relative = path.relative_to(self.root).as_posix()
        subprocess.run(
            ["git", "add", "--", relative],
            cwd=self.root, check=True,
        )
        subprocess.run(
            ["git", "-c", "user.name=TFA validator test",
             "-c", "user.email=tfa-validator@example.invalid", "commit", "-q",
             "--allow-empty", "-m", "fixture evidence", "--",
             relative, *(extra_paths or [])],
            cwd=self.root, check=True,
        )

    def rewrite_record(self, path: Path, record: dict) -> None:
        path.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")

    def assert_invalid(self, phrase: str) -> None:
        with self.assertRaises(validator.ValidationFailure) as caught:
            validator.validate_tree(self.root)
        self.assertIn(phrase, "\n".join(caught.exception.errors))

    def test_published_schema_has_nonempty_combinators(self) -> None:
        schema = json.loads((ROOT / "experiment.schema.json").read_text())
        self.assertEqual(schema["$schema"], "http://json-schema.org/draft-07/schema#")

        def walk(value: object, path: str) -> None:
            if isinstance(value, dict):
                for key, child in value.items():
                    if key in {"allOf", "anyOf", "oneOf"}:
                        self.assertIsInstance(child, list, path + "." + key)
                        self.assertGreater(len(child), 0, path + "." + key)
                    walk(child, path + "." + key)
            elif isinstance(value, list):
                for index, child in enumerate(value):
                    walk(child, f"{path}[{index}]")

        walk(schema, "$")

    def test_valid_planned_record_and_denominator(self) -> None:
        self.write_record(planned_record(self.entry))
        self.assertEqual(
            validator.validate_tree(self.root),
            {"cohort_targets": 30, "records": 1, "planned": 1, "A": 0, "W": 0, "B": 0, "U": 0, "S": 0, "C": 0, "T": 0, "R": 0, "conclusion": "neither", "evidence_authentication": "NOT_CHECKED"},
        )

    def test_valid_early_working_bracket_does_not_require_n(self) -> None:
        self.write_record(working_record(self.entry))
        counts = validator.validate_tree(self.root)
        self.assertEqual((counts["A"], counts["W"], counts["S"], counts["C"], counts["T"], counts["R"]), (1, 1, 2, 0, 2, 4))

    def test_registry_identity_and_ranking_are_exact(self) -> None:
        for mutate, phrase in (
            (lambda r: r["identity"].update(test_name="other"), "identity.test_name disagrees"),
            (lambda r: r["candidate_ranking"]["pfs"].update(value=0.1), "candidate_ranking disagrees"),
        ):
            with self.subTest(phrase=phrase):
                record = planned_record(self.entry)
                mutate(record)
                self.write_record(record)
                self.assert_invalid(phrase)

    def test_cohort_digest_and_metadata_are_exact(self) -> None:
        cohort = copy.deepcopy(self.cohort)
        cohort["counting_key"] = "testinfra_id"
        (self.root / "cohort.json").write_text(json.dumps(cohort))
        self.assert_invalid("counting_key")

    def test_json_boolean_is_not_integer(self) -> None:
        record = working_record(self.entry)
        record["cells"][0]["hermit"]["strict"] = 1
        record["cells"][0]["cell_sha256"] = validator.canonical_object_sha256(record["cells"][0], "cell_sha256")
        self.write_record(record)
        self.assert_invalid("must equal True")

    def test_route_cell_event_identity_digests_bind(self) -> None:
        mutations = (
            (lambda r: r["plan"]["authorized_routes"][0].update(canonical_target="fbcode//folly:x"), "canonical_target disagrees"),
            (lambda r: r["cells"][0].update(route_sha256="3" * 64), "does not bind route_sha256"),
            (lambda r: r["events"][0].update(source_revision="4" * 40), "source revision disagrees"),
        )
        for mutate, phrase in mutations:
            with self.subTest(phrase=phrase):
                record = working_record(self.entry)
                mutate(record)
                self.write_record(record)
                self.assert_invalid(phrase)

    def test_exact_argv_blocks_guest_flag_smuggling_and_duplicates(self) -> None:
        mutations = (
            (lambda a: a.remove("--strict"), "exactly one explicit --strict"),
            (lambda a: (a.remove("--strict"), a.insert(a.index("--") + 1, "--strict")), "exactly one explicit --strict"),
            (lambda a: a.insert(a.index("--"), "--backend=ptrace"), "exactly one explicit ptrace"),
            (lambda a: a.insert(a.index("--"), "--rng-seed=0"), "--rng-seed must occur exactly once"),
            (lambda a: a.insert(a.index("--"), "--no-deterministic-io"), "disallowed relaxation"),
        )
        for mutate, phrase in mutations:
            with self.subTest(phrase=phrase):
                record = working_record(self.entry)
                mutate(record["events"][0]["argv"])
                self.write_record(record)
                self.assert_invalid(phrase)

    def test_planned_route_rejects_missing_seed_and_relaxing_modes(self) -> None:
        record = planned_record(self.entry)
        route = record["plan"]["authorized_routes"][0]
        index = route["launch_argv"].index("--rng-seed")
        del route["launch_argv"][index:index + 2]
        route["route_sha256"] = validator.canonical_object_sha256(route, "route_sha256")
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(
            record["plan"], "plan_sha256"
        )
        self.write_record(record)
        self.assert_invalid("--rng-seed must occur exactly once")

        for option in (
            "--no-namespace",
            "--core-only",
            "--lite",
            "--network=host",
            "--net=host",
            "--debug-futex-mode=external",
            "--no-virtualize-time",
        ):
            with self.subTest(option=option):
                record = planned_record(self.entry)
                route = record["plan"]["authorized_routes"][0]
                route["launch_argv"].insert(route["launch_argv"].index("--"), option)
                route["route_sha256"] = validator.canonical_object_sha256(route, "route_sha256")
                record["plan"]["plan_sha256"] = validator.canonical_object_sha256(
                    record["plan"], "plan_sha256"
                )
                self.write_record(record)
                self.assert_invalid("disallowed relaxation")

        for removed, expected in (
            (("--sched-heuristic", "random"), "supported --sched-heuristic"),
            (("--max-timeslice", "200000000"), "unsigned 64-bit --max-timeslice"),
            (("--verify",), "explicit --verify"),
            (("--verify-strict",), "explicit --verify-strict"),
            (("--verify-allow", "both"), "--verify-allow=both"),
        ):
            with self.subTest(removed=removed):
                record = planned_record(self.entry)
                route = record["plan"]["authorized_routes"][0]
                index = route["launch_argv"].index(removed[0])
                del route["launch_argv"][index:index + len(removed)]
                route["route_sha256"] = validator.canonical_object_sha256(
                    route, "route_sha256"
                )
                record["plan"]["plan_sha256"] = validator.canonical_object_sha256(
                    record["plan"], "plan_sha256"
                )
                self.write_record(record)
                self.assert_invalid(expected)

        record = planned_record(self.entry)
        route = record["plan"]["authorized_routes"][0]
        index = route["launch_argv"].index("--sh-deadline")
        misplaced = route["launch_argv"][index:index + 2]
        del route["launch_argv"][index:index + 2]
        run_index = route["launch_argv"].index("run")
        route["launch_argv"][run_index + 1:run_index + 1] = misplaced
        route["route_sha256"] = validator.canonical_object_sha256(
            route, "route_sha256"
        )
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(
            record["plan"], "plan_sha256"
        )
        self.write_record(record)
        self.assert_invalid("--sh-deadline must occur exactly once")

    def test_seed_values_must_fit_hermit_u64(self) -> None:
        record = planned_record(self.entry)
        design = record["plan"]["measurement_design"]
        design["seed_population"]["values"][0] = 1 << 64
        design["selection_order"]["seed_values"][0] = 1 << 64
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(
            record["plan"], "plan_sha256"
        )
        self.write_record(record)
        self.assert_invalid("must be at most 18446744073709551615")

    def test_event_requires_safehermit_and_exact_cell_template(self) -> None:
        record = working_record(self.entry)
        record["events"][0]["argv"][0] = "hermit"
        self.write_record(record)
        self.assert_invalid("safehermit")
        self.assert_invalid("drifts from cell template")

    def test_build_event_must_equal_sealed_route_command(self) -> None:
        record = blocked_record(self.entry)
        record["events"][0]["argv"].append("--changed")
        self.write_record(record)
        self.assert_invalid("BUILD event")


    def test_every_hermit_event_is_assigned_exactly_once(self) -> None:
        record = working_record(self.entry)
        record["samples"][0]["execution_event_ids"].pop()
        self.write_record(record)
        self.assert_invalid("must cover every Hermit event")
        self.assert_invalid("unassigned")

    def test_confirmation_event_does_not_increment_seed_counts(self) -> None:
        self.write_record(working_record(self.entry))
        counts = validator.validate_tree(self.root)
        self.assertEqual((counts["S"], counts["T"], counts["R"]), (2, 2, 4))

    def test_sample_requires_one_discovery_and_unique_tuple(self) -> None:
        record = working_record(self.entry)
        record["events"][1]["measurement_role"] = "DISCOVERY"
        duplicate = copy.deepcopy(record["samples"][0])
        duplicate["sample_id"] = "duplicate-sample"
        record["samples"].append(duplicate)
        self.write_record(record)
        self.assert_invalid("one DISCOVERY")
        self.assert_invalid("duplicate sample tuple")

    def test_sample_prefix_is_derived_from_selection_index(self) -> None:
        record = working_record(self.entry)
        record["samples"][1]["selection_index"] = 3
        self.write_record(record)
        self.assert_invalid("selection indexes must be zero-based prefix")

    def test_contradictory_same_tuple_invalidates_witness(self) -> None:
        record = working_record(self.entry)
        event = record["events"][1]
        event["result"]["target_oracle"] = "FAIL"
        event["result"]["failure_class_id"] = "assertion-1"
        event["result"]["exit_code"] = 1
        self.write_record(record)
        self.assert_invalid("disagrees with target outcome")
        self.assert_invalid("does not compare one agreeing tuple")

    def test_unique_invocation_replica_and_builtin_pairing(self) -> None:
        record = working_record(self.entry)
        record["events"][2]["invocation_id"] = record["events"][0]["invocation_id"]
        record["events"][2]["replica_index"] = record["events"][0]["replica_index"]
        self.write_record(record)
        self.assert_invalid("duplicate invocation/replica")

    def test_builtin_verify_requires_verify_flags(self) -> None:
        for flag, phrase in (
            ("--verify", "--verify --verify-strict"),
            ("--verify-strict", "--verify --verify-strict"),
            ("--verify-allow", "--verify-allow=both"),
        ):
            with self.subTest(flag=flag):
                record = working_record(self.entry)
                for event in record["events"][:2]:
                    index = event["argv"].index(flag)
                    del event["argv"][index:index + (2 if flag == "--verify-allow" else 1)]
                self.write_record(record)
                self.assert_invalid(phrase)

    def test_result_timeout_origin_and_oracle_are_consistent(self) -> None:
        record = working_record(self.entry)
        record["events"][0]["result"].update(timed_out=True, timeout_origin="OUTER", harness_status="TIMEOUT")
        self.write_record(record)
        self.assert_invalid("PASS is inconsistent")
        record = working_record(self.entry)
        record["events"][0]["result"]["selected_test_executed_count"] = 0
        self.write_record(record)
        self.assert_invalid("zero selected tests")

    def test_working_requires_all_three_streams_and_canonical_info(self) -> None:
        record = working_record(self.entry)
        record["events"][0]["artifacts"][0]["kind"] = "OTHER"
        self.write_record(record)
        self.assert_invalid("complete evidence bracket")
        record = working_record(self.entry)
        record["matchers"][0]["canonical_info_artifact"]["truncated"] = True
        self.write_record(record)
        self.assert_invalid("canonical INFO is not complete committed evidence")

    def test_tracked_artifact_must_exist_match_and_be_indexed(self) -> None:
        record = working_record(self.entry)
        path = self.write_record(record)
        item = record["events"][0]["artifacts"][0]
        (self.root / item["locator"]).unlink()
        self.assert_invalid("does not exist as a regular file")
        self.write_record(record)
        item["sha256"] = "0" * 64
        self.rewrite_record(path, record)
        self.assert_invalid("digest disagrees")

    def test_artifact_bounds_locator_uniqueness_and_external_scheme(self) -> None:
        record = working_record(self.entry)
        record["events"][0]["artifacts"][0]["capture_limit_bytes"] = 1
        record["events"][0]["artifacts"][1]["locator"] = record["events"][0]["artifacts"][0]["locator"]
        external = record["events"][0]["artifacts"][2]
        external["storage"] = "EXTERNAL_DURABLE"
        external["locator"] = "file:/tmp/result"
        self.write_record(record)
        self.assert_invalid("exceeds its capture bound")
        self.assert_invalid("duplicate artifact locator")
        self.assert_invalid("allowed durable URI scheme")

    def test_symlink_is_not_accepted_as_tracked_artifact(self) -> None:
        record = working_record(self.entry)
        path = self.write_record(record)
        item = record["events"][0]["artifacts"][0]
        candidate = self.root / item["locator"]
        target = self.root / "real-output"
        target.write_text("outside")
        candidate.unlink()
        candidate.symlink_to(target)
        subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.assert_invalid("unsafe locator or symlink")

    def test_external_labels_cannot_stand_in_for_working_payloads(self) -> None:
        record = working_record(self.entry)
        for event in record["events"]:
            for item in event["artifacts"]:
                if item["kind"] != "SAFEHERMIT_REPORT":
                    item["storage"] = "EXTERNAL_DURABLE"
                    item["locator"] = f"https://example.invalid/{item['artifact_id']}"
        for matcher in record["matchers"]:
            item = matcher["canonical_info_artifact"]
            item["storage"] = "EXTERNAL_DURABLE"
            item["locator"] = f"https://example.invalid/{item['artifact_id']}"
        self.write_record(record)
        self.assert_invalid("RESULT must be complete committed evidence")
        self.assert_invalid("canonical INFO is not complete committed evidence")

    def test_artifact_locators_are_unique_across_target_records(self) -> None:
        shared = "https://example.invalid/shared-diagnostic"
        for entry in self.cohort["targets"][:2]:
            record = unknown_record(entry)
            diagnostic = record["events"][0]["artifacts"][0]
            diagnostic.update(
                storage="EXTERNAL_DURABLE", locator=shared,
                sha256="4" * 64, bytes=1,
            )
            record["current_outcome"]["inspected_artifacts"] = [diagnostic["artifact_id"]]
            record["outcome_history"] = [copy.deepcopy(record["current_outcome"])]
            self.write_record(record)
        self.assert_invalid("artifact locator 'https://example.invalid/shared-diagnostic' is already owned")

    def test_result_and_canonical_info_payloads_bind_claims(self) -> None:
        record = working_record(self.entry)
        path = self.write_record(record)
        event = record["events"][0]
        item = next(a for a in event["artifacts"] if a["kind"] == "RESULT")
        claimed = copy.deepcopy(validator._event_result_payload(event))
        claimed["result"]["target_oracle"] = "FAIL"
        content = _json_payload(claimed)
        (self.root / item["locator"]).write_bytes(content)
        item["bytes"] = len(content)
        item["sha256"] = hashlib.sha256(content).hexdigest()
        subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.commit_record(path, [item["locator"]])
        self.assert_invalid("RESULT payload disagrees with event")

        record = working_record(self.entry)
        path = self.write_record(record)
        matcher = record["matchers"][0]
        item = matcher["canonical_info_artifact"]
        claimed = copy.deepcopy(validator._matcher_info_payload(matcher))
        claimed["details"]["verified"] = False
        content = _json_payload(claimed)
        (self.root / item["locator"]).write_bytes(content)
        item["bytes"] = len(content)
        item["sha256"] = hashlib.sha256(content).hexdigest()
        subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.commit_record(path, [item["locator"]])
        self.assert_invalid("canonical INFO payload disagrees with matcher")


    def exhausted_unknown(self, entry: dict, split_fail: bool = False) -> dict:
        record = working_record(entry)
        n = record["plan"]["measurement_design"]["required_valid_seed_tuples"]
        seeds = record["plan"]["measurement_design"]["selection_order"]["seed_values"][:n]
        template_event = copy.deepcopy(record["events"][0])
        events, samples = [], []
        for index, seed in enumerate(seeds):
            event = copy.deepcopy(template_event)
            event_id = f"event-discovery-{index}"
            event["event_id"] = event_id
            event["measurement_role"] = "DISCOVERY"
            event["invocation_id"] = f"discovery-{index}"
            event["replica_index"] = 1
            event["seed"] = {"flag": "--sched-seed", "value": seed}
            event["argv"] = validator._replace_flag_value(record["cells"][0]["hermit"]["argv"], "--sched-seed", str(seed))
            outcome = "FAIL" if split_fail and index == n - 1 else "PASS"
            event["result"]["target_oracle"] = outcome
            event["result"]["failure_class_id"] = "assertion-1" if outcome == "FAIL" else None
            event["result"]["exit_code"] = 1 if outcome == "FAIL" else 0
            event["artifacts"] = [
                artifact(f"{event_id}-result", "RESULT"),
                artifact(f"{event_id}-safehermit", "SAFEHERMIT_REPORT"),
            ]
            event["argv"] = [event["artifacts"][1]["locator"] if token == "{SAFEHERMIT_REPORT}" else token for token in event["argv"]]
            events.append(event)
            samples.append({
                "sample_id": f"sample-{index}",
                "cell_id": "cell-1",
                "seed": {"flag": "--sched-seed", "value": seed},
                "selection_index": index,
                "status": "VALID",
                "target_outcome": outcome,
                "failure_class_id": "assertion-1" if outcome == "FAIL" else None,
                "censor_reason": None,
                "execution_event_ids": [event_id],
            })
        outcome = {
            "classification_id": "classification-unknown",
            "classified_at": "2026-09-18T04:02:00Z",
            "supersedes": None,
            "evidence_refs": [event["event_id"] for event in events],
            "kind": "NOT-DETERMINABLE",
            "route_evidence": [{"route_id": "route-primary", "cell_ids": ["cell-1"], "evidence_event_ids": [event["event_id"] for event in events]}],
            "inspected_commands": [copy.deepcopy(event["argv"]) for event in events],
            "inspected_artifacts": [event["artifacts"][0]["artifact_id"] for event in events],
            "missing_or_ambiguous_facts": ["only one arm" if not split_fail else "repeat not complete"],
            "measurement_budget_exhausted": True,
            "pending_in_scope_measurements": [],
        }
        record.update(events=events, samples=samples, matchers=[], outcome_history=[outcome], current_outcome=copy.deepcopy(outcome))
        return record

    def test_blocker_requires_exact_route_evidence_and_owner(self) -> None:
        self.write_record(blocked_record(self.entry))
        self.assertEqual(validator.validate_tree(self.root)["B"], 1)
        record = blocked_record(self.entry)
        record["current_outcome"]["cause"]["campaign_owner_adjudication"]["owner_identity"] = "someone-else"
        record["outcome_history"] = [copy.deepcopy(record["current_outcome"])]
        self.write_record(record)
        self.assert_invalid("not from campaign owner")

    def test_campaign_owner_is_pinned_by_campaign_metadata(self) -> None:
        record = planned_record(self.entry)
        record["plan"]["campaign_owner_identity"] = "self-appointed"
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(
            record["plan"], "plan_sha256"
        )
        self.write_record(record)
        self.assert_invalid("campaign_owner_identity differs from campaign metadata")

        metadata_path = self.root / "metadata.json"
        metadata = json.loads(metadata_path.read_text())
        metadata["campaign_owner_identity"] = "self-appointed"
        metadata_path.write_text(json.dumps(metadata, indent=2) + "\n")
        record = blocked_record(self.entry)
        record["plan"]["campaign_owner_identity"] = "self-appointed"
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(
            record["plan"], "plan_sha256"
        )
        for event in record["events"]:
            event["plan_sha256"] = record["plan"]["plan_sha256"]
        adjudication = record["current_outcome"]["cause"]["campaign_owner_adjudication"]
        adjudication["owner_identity"] = "self-appointed"
        record["outcome_history"] = [copy.deepcopy(record["current_outcome"])]
        self.write_record(record)
        self.assert_invalid("metadata.json differs from the Git index")

    def test_material_plan_receipt_requires_exact_prior_planned_record(self) -> None:
        record = working_record(self.entry)
        path = self.write_record(record)
        record["plan_receipt"]["commit"] = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=self.root, check=True,
            capture_output=True, text=True,
        ).stdout.strip()
        record["plan_receipt"]["blob"] = subprocess.run(
            ["git", "rev-parse", f"HEAD:{record['plan_receipt']['path']}"],
            cwd=self.root, check=True, capture_output=True, text=True,
        ).stdout.strip()
        self.rewrite_record(path, record)
        self.commit_record(path)
        self.assert_invalid("does not contain the exact pre-run PLANNED record")

    def test_blocker_diagnostic_and_owner_payloads_bind_claims(self) -> None:
        record = blocked_record(self.entry)
        path = self.write_record(record)
        event = record["events"][0]
        diagnostic = next(a for a in event["artifacts"] if a["kind"] == "DIAGNOSTIC")
        content = b"different diagnostic\n"
        (self.root / diagnostic["locator"]).write_bytes(content)
        diagnostic["bytes"] = len(content)
        diagnostic["sha256"] = hashlib.sha256(content).hexdigest()
        subprocess.run(["git", "add", "--", diagnostic["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.commit_record(path, [diagnostic["locator"]])
        self.assert_invalid("diagnostic artifact disagrees with cited event")

        record = blocked_record(self.entry)
        path = self.write_record(record)
        adjudication = record["current_outcome"]["cause"]["campaign_owner_adjudication"]
        item = next(
            artifact_item for artifact_item in record["events"][0]["artifacts"]
            if artifact_item["artifact_id"] == adjudication["evidence_artifact_id"]
        )
        claimed = {
            "schema_version": 1,
            **{key: value for key, value in adjudication.items() if key != "evidence_artifact_id"},
        }
        claimed["owner_identity"] = "self-appointed"
        content = _json_payload(claimed)
        (self.root / item["locator"]).write_bytes(content)
        item["bytes"] = len(content)
        item["sha256"] = hashlib.sha256(content).hexdigest()
        subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.commit_record(path, [item["locator"]])
        self.assert_invalid("adjudication payload disagrees with owner decision")

    def test_blocker_cannot_cite_success_and_truncated_diagnostic_is_allowed(self) -> None:
        record = blocked_record(self.entry)
        record["events"][0]["artifacts"][0]["truncated"] = True
        self.write_record(record)
        self.assertEqual(validator.validate_tree(self.root)["B"], 1)
        record = blocked_record(self.entry)
        result = record["events"][0]["result"]
        result.update(exit_code=0, guest_started=False, selected_test_executed_count=0, target_oracle="NOT_RUN", harness_status="COMPLETE")
        self.write_record(record)
        self.assert_invalid("cites successful attempt evidence")

    def test_zero_sample_unknown_records_never_make_negative(self) -> None:
        for entry in self.cohort["targets"]:
            self.write_record(unknown_record(entry))
        counts = validator.validate_tree(self.root)
        self.assertEqual((counts["A"], counts["U"], counts["S"], counts["T"]), (30, 30, 0, 0))
        self.assertEqual(counts["conclusion"], "neither")

    def test_unknown_exhaustion_is_derived_and_repeat_budget_matters(self) -> None:
        record = unknown_record(self.entry)
        record["current_outcome"]["measurement_budget_exhausted"] = True
        record["current_outcome"]["pending_in_scope_measurements"] = []
        record["outcome_history"] = [copy.deepcopy(record["current_outcome"])]
        self.write_record(record)
        self.assert_invalid("exhaustion boolean disagrees")
        record = self.exhausted_unknown(self.entry)
        self.write_record(record)
        self.assertEqual(validator.validate_tree(self.root)["U"], 1)
        record = self.exhausted_unknown(self.entry, split_fail=True)
        self.write_record(record)
        self.assert_invalid("exhaustion boolean disagrees")

    def test_required_n_and_default_miss_probability_are_derived(self) -> None:
        record = planned_record(self.entry)
        record["plan"]["measurement_design"]["required_valid_seed_tuples"] += 1
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(record["plan"], "plan_sha256")
        self.write_record(record)
        self.assert_invalid("required N must be derived")
        record = planned_record(self.entry)
        record["plan"]["measurement_design"]["design_basis"]["max_miss_probability"] = 0.10
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(record["plan"], "plan_sha256")
        self.write_record(record)
        self.assert_invalid("default/owner override")

    def test_dated_owner_override_can_change_miss_probability(self) -> None:
        record = planned_record(self.entry)
        design = record["plan"]["measurement_design"]
        design["miss_probability_override"] = {
            "max_miss_probability": 0.10,
            "owner_identity": "rrnewton",
            "decided_at": "2026-09-18T03:00:00Z",
            "authority": "CAMPAIGN_OWNER",
            "rationale": "synthetic explicit decision",
        }
        design["design_basis"]["max_miss_probability"] = 0.10
        design["required_valid_seed_tuples"] = _minimum_n(0.25, 0.10)
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(record["plan"], "plan_sha256")
        self.write_record(record)
        self.assertEqual(validator.validate_tree(self.root)["planned"], 1)

    def test_guest_started_alone_controls_execution_count(self) -> None:
        record = unknown_record(self.entry)
        record["events"][0]["kind"] = "HERMIT_EXECUTION"
        route = record["plan"]["authorized_routes"][0]
        record["cells"] = [working_record(self.entry)["cells"][0]]
        cell = record["cells"][0]
        event = record["events"][0]
        event["artifacts"].append(artifact("event-inspection-safehermit", "SAFEHERMIT_REPORT"))
        event.update(
            cell_id="cell-1", cell_sha256=cell["cell_sha256"],
            measurement_role="DISCOVERY", invocation_id="did-not-start",
            replica_index=1, seed={"flag": "--sched-seed", "value": 7},
            effective_config_sha256=cell["cell_sha256"],
            argv=validator._replace_flag_value(route["launch_argv"], "--sched-seed", "7"),
        )
        event["argv"] = [
            event["artifacts"][-1]["locator"] if token == "{SAFEHERMIT_REPORT}" else token
            for token in event["argv"]
        ]
        outcome = record["current_outcome"]
        outcome["inspected_commands"] = [copy.deepcopy(event["argv"])]
        record["outcome_history"] = [copy.deepcopy(outcome)]
        record["samples"] = [{
            "sample_id": "censored", "cell_id": "cell-1",
            "seed": {"flag": "--sched-seed", "value": 7}, "selection_index": 0,
            "status": "CENSORED", "target_outcome": None, "failure_class_id": None,
            "censor_reason": "guest never started", "execution_event_ids": [event["event_id"]],
        }]
        self.write_record(record)
        self.assertEqual(validator.validate_tree(self.root)["R"], 0)

    def test_thirty_outside_scope_blockers_are_negative(self) -> None:
        for entry in self.cohort["targets"]:
            self.write_record(blocked_record(entry))
        counts = validator.validate_tree(self.root)
        self.assertEqual((counts["A"], counts["B"], counts["W"]), (30, 30, 0))
        self.assertEqual(counts["conclusion"], "NEGATIVE RESULT")


    def test_nonexhausted_unknown_must_name_pending_work(self) -> None:
        record = unknown_record(self.entry)
        record["current_outcome"]["pending_in_scope_measurements"] = []
        record["outcome_history"] = [copy.deepcopy(record["current_outcome"])]
        self.write_record(record)
        self.assert_invalid("must name pending measurement")

    def test_unknown_inspection_claims_bind_to_cited_events(self) -> None:
        record = unknown_record(self.entry)
        current = record["current_outcome"]
        current["inspected_commands"] = [["invented", "command"]]
        record["outcome_history"] = [copy.deepcopy(current)]
        self.write_record(record)
        self.assert_invalid("inspected_commands must equal cited event commands")

        record = unknown_record(self.entry)
        extra = preguest_event(record, "event-not-cited")
        record["events"].append(extra)
        current = record["current_outcome"]
        current["inspected_artifacts"] = [extra["artifacts"][0]["artifact_id"]]
        record["outcome_history"] = [copy.deepcopy(current)]
        self.write_record(record)
        self.assert_invalid("not produced by cited route evidence")

        record = unknown_record(self.entry)
        current = record["current_outcome"]
        current["evidence_refs"] = [current["inspected_artifacts"][0]]
        record["outcome_history"] = [copy.deepcopy(current)]
        self.write_record(record)
        self.assert_invalid("evidence_refs omit inspected event")

    def test_launch_and_guest_environment_bind_to_route(self) -> None:
        record = blocked_record(self.entry)
        record["events"][0]["kind"] = "LAUNCH"
        self.write_record(record)
        self.assert_invalid("LAUNCH event")
        record = working_record(self.entry)
        record["cells"][0]["guest"]["environment"] = {"LC_ALL": "other"}
        self.write_record(record)
        self.assert_invalid("guest environment disagrees")

    def test_matcher_kind_details_pair_and_effective_config_bind(self) -> None:
        record = working_record(self.entry)
        record["matchers"][0]["kind"] = "EQUIVALENT"
        record["events"][0]["effective_config_sha256"] = "7" * 64
        self.write_record(record)
        self.assert_invalid("EQUIVALENT details mismatch")
        self.assert_invalid("kind differs from sealed route")
        self.assert_invalid("effective config differs")

    def test_equivalent_comparator_is_sealed_before_events(self) -> None:
        record = working_record(self.entry, "EQUIVALENT")
        self.write_record(record)
        self.assertEqual(validator.validate_tree(self.root)["W"], 1)

        record = working_record(self.entry, "EQUIVALENT")
        for matcher in record["matchers"]:
            matcher["details"]["comparator_argv"] = ["chosen-after-results"]
            matcher["details"]["comparator_sha256"] = "f" * 64
        self.write_record(record)
        self.assert_invalid("comparator differs from sealed route")

    def test_declared_scheduler_modes_equal_actual_options(self) -> None:
        record = working_record(self.entry)
        insert_at = record["events"][0]["argv"].index("--")
        record["events"][0]["argv"].insert(insert_at, "--fuzz-futexes")
        self.write_record(record)
        self.assert_invalid("scheduler modes")



    def test_safehermit_path_and_structured_scheduler_flags_are_exact(self) -> None:
        record = working_record(self.entry)
        for event in record["events"]:
            event["argv"][0] = "/evil/safehermit"
        self.write_record(record)
        self.assert_invalid("route through bin/safehermit")
        record = working_record(self.entry)
        index = record["events"][0]["argv"].index("--sched-heuristic")
        record["events"][0]["argv"][index + 1] = "connectbind"
        self.write_record(record)
        self.assert_invalid("drifts from cell template")
        record = working_record(self.entry)
        record["cells"][0]["hermit"]["scheduler_heuristic"] = "connectbind"
        self.write_record(record)
        self.assert_invalid("scheduler_heuristic disagrees")

    def test_blocker_stage_diagnostic_and_time_bind_to_evidence(self) -> None:
        record = blocked_record(self.entry)
        cause = record["current_outcome"]["cause"]
        cause["stage"] = "REPEAT"
        cause["diagnostic"]["kind"] = "ORACLE_AMBIGUITY"
        cause["diagnostic"]["message"] = "unrelated"
        cause["campaign_owner_adjudication"]["decided_at"] = "2020-01-01T00:00:00Z"
        record["outcome_history"] = [copy.deepcopy(record["current_outcome"])]
        self.write_record(record)
        self.assert_invalid("stage disagrees")
        self.assert_invalid("diagnostic does not match")
        self.assert_invalid("predates cited evidence")



    def test_verify_flags_cannot_be_smuggled_into_guest_argv(self) -> None:
        record = working_record(self.entry)
        route = record["plan"]["authorized_routes"][0]
        argv = route["launch_argv"]
        for flag in ("--verify", "--verify-strict"):
            argv.remove(flag)
            argv.insert(argv.index("--") + 1, flag)
        route["route_sha256"] = validator.canonical_object_sha256(route, "route_sha256")
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(record["plan"], "plan_sha256")
        cell = record["cells"][0]
        cell["route_sha256"] = route["route_sha256"]
        cell["plan_sha256"] = record["plan"]["plan_sha256"]
        cell["hermit"]["argv"] = copy.deepcopy(route["launch_argv"])
        cell["guest"]["argv"] = cell["hermit"]["argv"][cell["hermit"]["argv"].index("--") + 1:]
        cell["cell_sha256"] = validator.canonical_object_sha256(cell, "cell_sha256")
        for event in record["events"]:
            event["route_sha256"] = route["route_sha256"]
            event["plan_sha256"] = record["plan"]["plan_sha256"]
            event["cell_sha256"] = cell["cell_sha256"]
            event["effective_config_sha256"] = cell["cell_sha256"]
            event["argv"] = validator._replace_flag_value(cell["hermit"]["argv"], "--sched-seed", str(event["seed"]["value"]))
        self.write_record(record)
        self.assert_invalid("requires exact --verify --verify-strict")

    def test_pmu_fallback_records_requested_and_effective_timeslice(self) -> None:
        record = working_record(self.entry)
        cell = record["cells"][0]
        cell["pmu"].update(available=False, effective_timer="DISABLED", fallback_recorded=True)
        cell["timing"]["effective_max_timeslice"] = "disabled"
        cell["cell_sha256"] = validator.canonical_object_sha256(cell, "cell_sha256")
        for event in record["events"]:
            event["cell_sha256"] = cell["cell_sha256"]
            event["effective_config_sha256"] = cell["cell_sha256"]
        self.write_record(record)
        self.assertEqual(validator.validate_tree(self.root)["W"], 1)



    def test_target_timeout_requires_predicate_and_declared_class(self) -> None:
        record = planned_record(self.entry)
        record["plan"]["target_oracle"]["timeout_policy"]["target_timeout_can_fail"] = True
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(record["plan"], "plan_sha256")
        self.write_record(record)
        self.assert_invalid("requires predicate and declared timeout failure class")


    def test_safehermit_must_invoke_run_and_report_binary_hash(self) -> None:
        record = working_record(self.entry)
        route = record["plan"]["authorized_routes"][0]
        route["launch_argv"][route["launch_argv"].index("run")] = "echo"
        route["route_sha256"] = validator.canonical_object_sha256(route, "route_sha256")
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(record["plan"], "plan_sha256")
        cell = record["cells"][0]
        cell["route_sha256"] = route["route_sha256"]
        cell["plan_sha256"] = record["plan"]["plan_sha256"]
        cell["hermit"]["argv"] = copy.deepcopy(route["launch_argv"])
        cell["cell_sha256"] = validator.canonical_object_sha256(cell, "cell_sha256")
        for event in record["events"]:
            event["route_sha256"] = route["route_sha256"]
            event["plan_sha256"] = record["plan"]["plan_sha256"]
            event["cell_sha256"] = cell["cell_sha256"]
            event["effective_config_sha256"] = cell["cell_sha256"]
            event["argv"] = validator._replace_flag_value(
                cell["hermit"]["argv"], "--sched-seed", str(event["seed"]["value"])
            )
        self.write_record(record)
        self.assert_invalid("run subcommand")

        record = working_record(self.entry)
        path = self.write_record(record)
        report = next(
            item for item in record["events"][0]["artifacts"]
            if item["kind"] == "SAFEHERMIT_REPORT"
        )
        report_path = self.root / report["locator"]
        content = report_path.read_bytes().replace(
            record["cells"][0]["hermit_binary_sha256"].encode(),
            ("3" * 64).encode(),
        )
        report_path.write_bytes(content)
        report["bytes"] = len(content)
        report["sha256"] = hashlib.sha256(content).hexdigest()
        subprocess.run(["git", "add", "--", report["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.commit_record(path, [report["locator"]])
        self.assert_invalid("does not bind hermit_binary_sha256")

        record = working_record(self.entry)
        path = self.write_record(record)
        report = next(
            item for item in record["events"][0]["artifacts"]
            if item["kind"] == "SAFEHERMIT_REPORT"
        )
        content = ("safehermit: binary_sha256=" + record["cells"][0]["hermit_binary_sha256"] + "\n").encode()
        (self.root / report["locator"]).write_bytes(content)
        report["bytes"] = len(content)
        report["sha256"] = hashlib.sha256(content).hexdigest()
        subprocess.run(["git", "add", "--", report["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.commit_record(path, [report["locator"]])
        self.assert_invalid("missing required fields")

        record = working_record(self.entry)
        path = self.write_record(record)
        report = next(
            item for item in record["events"][0]["artifacts"]
            if item["kind"] == "SAFEHERMIT_REPORT"
        )
        report_path = self.root / report["locator"]
        content = report_path.read_bytes().replace(
            b"safehermit: bound.wall=APPLIED:60s\n",
            b"safehermit: bound.wall=NOT_APPLIED:synthetic\n",
        )
        report_path.write_bytes(content)
        report["bytes"] = len(content)
        report["sha256"] = hashlib.sha256(content).hexdigest()
        subprocess.run(["git", "add", "--", report["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.commit_record(path, [report["locator"]])
        self.assert_invalid("wall bound differs")

    def test_safehermit_accepts_symbolic_signal_result(self) -> None:
        record = working_record(self.entry)
        event = record["events"][0]
        event["result"].update(
            exit_code=132,
            signal="SIGILL",
            selected_test_executed_count=0,
            target_oracle="NOT_RUN",
            failure_class_id=None,
            harness_status="ERROR",
        )
        report = next(
            item for item in event["artifacts"]
            if item["kind"] == "SAFEHERMIT_REPORT"
        )
        payload = artifact_payloads(record)[report["artifact_id"]]
        errors = validator._safehermit_report_errors(
            payload,
            record["cells"][0],
            event["result"],
            record["plan"]["resource_bounds"],
            "signal-report",
        )
        self.assertEqual(errors, [])

    def test_tracked_artifact_worktree_must_equal_git_index(self) -> None:
        record = working_record(self.entry)
        path = self.write_record(record)
        item = record["events"][0]["artifacts"][0]
        changed = b"changed after staging\n"
        (self.root / item["locator"]).write_bytes(changed)
        item["bytes"] = len(changed)
        item["sha256"] = hashlib.sha256(changed).hexdigest()
        self.rewrite_record(path, record)
        self.assert_invalid("differs from the Git index")

    def test_staged_only_artifact_is_not_durable(self) -> None:
        record = working_record(self.entry)
        path = self.write_record(record)
        item = record["events"][0]["artifacts"][0]
        changed = b"staged but not committed\n"
        (self.root / item["locator"]).write_bytes(changed)
        item["bytes"] = len(changed)
        item["sha256"] = hashlib.sha256(changed).hexdigest()
        subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.assert_invalid("Git index differs from committed HEAD")

    def test_material_record_must_equal_committed_head(self) -> None:
        record = working_record(self.entry)
        path = self.write_record(record)
        current = record["current_outcome"]
        current["classified_at"] = "2026-09-18T04:03:00Z"
        record["outcome_history"] = [copy.deepcopy(current)]
        self.rewrite_record(path, record)
        self.assert_invalid("material record differs from the Git index")

    def test_working_requires_nonempty_result_artifact(self) -> None:
        record = working_record(self.entry)
        item = next(a for a in record["events"][0]["artifacts"] if a["kind"] == "RESULT")
        path = self.write_record(record)
        item["bytes"] = 0
        item["sha256"] = hashlib.sha256(b"").hexdigest()
        artifact_path = self.root / item["locator"]
        artifact_path.write_bytes(b"")
        subprocess.run(["git", "add", "--", item["locator"]], cwd=self.root, check=True)
        self.rewrite_record(path, record)
        self.assert_invalid("PASS witness does not meet complete evidence bracket")

    def test_extreme_rate_is_bounded_by_explicit_seed_population(self) -> None:
        record = planned_record(self.entry)
        basis = record["plan"]["measurement_design"]["design_basis"]
        basis["expected_hermit_fail_probability"] = 1e-20
        record["plan"]["plan_sha256"] = validator.canonical_object_sha256(record["plan"], "plan_sha256")
        self.write_record(record)
        self.assert_invalid("explicit seed population cannot meet miss bound")


if __name__ == "__main__":
    unittest.main()
