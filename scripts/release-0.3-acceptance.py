#!/usr/bin/env python3
"""Evaluate the Hermit 0.3 release contract from raw, exact-RC artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Mapping, Sequence

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "ci-hub/validate"))

import service_result  # noqa: E402


GO = 0
DEPLOYMENT_DEFECT = 2
FAIL = 3
NO_RESULT = 4
VERIFIED_BASE_SHA = "d53550510d1e7d13e84cc8af9bb90269e90b3f07"
PARENT_CONTRACT_BASE_SHA = "ec33089a26e0270464cf53092f13debf29243482"
SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


class ContractError(ValueError):
    pass


@dataclass(frozen=True)
class Subject:
    parent_sha: str
    hermit_sha: str
    reverie_sha: str

    def __post_init__(self) -> None:
        if any(not SHA40.fullmatch(value) for value in self.as_dict().values()):
            raise ContractError("subject requires lowercase 40-hex parent/Hermit/Reverie SHAs")

    def as_dict(self) -> dict[str, str]:
        return {
            "parent_sha": self.parent_sha,
            "hermit_sha": self.hermit_sha,
            "reverie_sha": self.reverie_sha,
        }


@dataclass
class Result:
    criterion: Mapping[str, Any]
    verdict: str
    reason: str
    observed: Mapping[str, Any] = field(default_factory=dict)

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.criterion["id"],
            "title": self.criterion["title"],
            "verdict": self.verdict,
            "reason": self.reason,
            "command": self.criterion["command"],
            "fields": [
                "subject.parent_sha", "subject.hermit_sha", "subject.reverie_sha",
                "authority.runner", "authority.runner_identity",
                "authority.signer", "authority.signer_identity",
                *self.criterion["fields"],
            ],
            "denominator": self.criterion["denominator"],
            "threshold": self.criterion["threshold"],
            "forbidden": self.criterion["forbidden"],
            "runner": self.criterion["producer"]["runner"],
            "signer": self.criterion["producer"]["signer"],
            "observed": self.observed,
        }


@dataclass
class Report:
    contract: Mapping[str, Any]
    subject: Subject
    tested_rc_is_current_main: bool
    results: list[Result] = field(default_factory=list)
    deployment_errors: list[str] = field(default_factory=list)

    @property
    def exit_code(self) -> int:
        if self.deployment_errors:
            return DEPLOYMENT_DEFECT
        if any(result.verdict == "FAIL" for result in self.results):
            return FAIL
        if any(result.verdict != "PASS" for result in self.results):
            return NO_RESULT
        return GO

    def as_dict(self) -> dict[str, Any]:
        manual = next((r for r in self.results if r.criterion["id"] == "AC-STRICT-03"), None)
        if manual and manual.verdict == "PASS":
            rc_status = "exact tested RC manual validation passed"
        elif self.tested_rc_is_current_main and self.subject.hermit_sha != VERIFIED_BASE_SHA:
            rc_status = "green at d5355051, current main unverified"
        elif self.subject.hermit_sha != VERIFIED_BASE_SHA:
            rc_status = "green at d5355051, tested RC unverified"
        else:
            rc_status = "tested RC is the verified d5355051 boundary, but exact manual evidence is still required"
        counts = {name: sum(r.verdict == name for r in self.results) for name in ("PASS", "FAIL", "NO_RESULT")}
        return {
            "schema": "hermit-release-acceptance-report-v2",
            "verified_hermit_base_sha": VERIFIED_BASE_SHA,
            "parent_contract_base_sha": PARENT_CONTRACT_BASE_SHA,
            "parent_status": "main unverified",
            "tested_rc_sha": self.subject.hermit_sha,
            "hermit_baseline_status": "green at d5355051",
            "tested_rc_status": rc_status,
            "subject": self.subject.as_dict(),
            "decision": {0: "GO", 2: "DEPLOYMENT_DEFECT", 3: "FAIL", 4: "NO_RESULT"}[self.exit_code],
            "exit_code": self.exit_code,
            "counts": {**counts, "total": len(self.results)},
            "deployment_errors": self.deployment_errors,
            "criteria": [result.as_dict() for result in self.results],
        }


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read JSON {path}: {error}") from error
    if not isinstance(value, dict):
        raise ContractError(f"JSON root is not an object: {path}")
    return value


def validate_contract(contract: Mapping[str, Any], repo_root: Path) -> list[str]:
    errors: list[str] = []
    if contract.get("verified_hermit_base_sha") != VERIFIED_BASE_SHA:
        errors.append(f"verified_hermit_base_sha must be {VERIFIED_BASE_SHA}")
    if contract.get("parent_contract_base_sha") != PARENT_CONTRACT_BASE_SHA:
        errors.append(f"parent_contract_base_sha must be {PARENT_CONTRACT_BASE_SHA}")
    if contract.get("parent_status") != "main unverified":
        errors.append("parent_status must remain main unverified")
    controls = contract.get("mutation_controls")
    if not isinstance(controls, Mapping) or controls.get("coverage") != "all-criteria":
        errors.append("mutation controls do not cover all criteria")
    criteria = contract.get("criteria")
    if not isinstance(criteria, list) or len(criteria) != 26:
        return errors + ["contract must contain exactly 26 criteria"]
    runners = contract.get("runners", {})
    signers = contract.get("signers", {})
    seen: set[str] = set()
    for registry_name, registry in (("runners", runners), ("signers", signers)):
        if not isinstance(registry, Mapping) or not registry:
            errors.append(f"{registry_name} registry is absent")
            continue
        for name, entry in registry.items():
            if not isinstance(entry, Mapping) or not str(entry.get("identity") or "").strip():
                errors.append(f"{registry_name}.{name} has no authority identity")
                continue
            probes = entry.get("probe_paths")
            if not isinstance(probes, list) or not probes:
                errors.append(f"{registry_name}.{name} has no deployment probe")
            elif any(not (repo_root / str(path)).exists() for path in probes):
                errors.append(f"{registry_name}.{name} runner/signer is not deployed")
    for criterion in criteria:
        criterion_id = criterion.get("id") if isinstance(criterion, Mapping) else None
        if not isinstance(criterion_id, str) or criterion_id in seen:
            errors.append(f"invalid or duplicate criterion id {criterion_id!r}")
            continue
        seen.add(criterion_id)
        command = criterion.get("command")
        fields = criterion.get("fields")
        producer = criterion.get("producer", {})
        if not isinstance(command, list) or not command or not all(isinstance(x, str) and x for x in command):
            errors.append(f"{criterion_id}: command argv is absent")
        if not isinstance(fields, list) or not fields or not all(isinstance(x, str) and x for x in fields):
            errors.append(f"{criterion_id}: field selectors are absent")
        if not isinstance(criterion.get("denominator"), Mapping):
            errors.append(f"{criterion_id}: denominator is absent")
        if not isinstance(criterion.get("threshold"), Mapping):
            errors.append(f"{criterion_id}: threshold is absent")
        if not isinstance(criterion.get("forbidden"), list) or not criterion["forbidden"]:
            errors.append(f"{criterion_id}: forbidden state is absent")
        if producer.get("runner") not in runners:
            errors.append(f"{criterion_id}: runner does not exist")
        if producer.get("signer") not in signers:
            errors.append(f"{criterion_id}: signer does not exist")
    return errors


def artifact_bytes(
    reference: Any, base: Path, expected_type: str
) -> tuple[bytes | None, str | None]:
    if not isinstance(reference, Mapping) or reference.get("type") != expected_type:
        return None, f"missing {expected_type} artifact reference"
    raw_path = reference.get("path")
    digest = reference.get("sha256")
    if not isinstance(raw_path, str) or not raw_path or not SHA256.fullmatch(str(digest or "")):
        return None, "artifact path or sha256 is absent"
    path = (base / raw_path).resolve()
    if not path.is_relative_to(base.resolve()):
        return None, "artifact path escapes the evidence directory"
    try:
        blob = path.read_bytes()
    except OSError:
        return None, f"raw artifact does not exist: {raw_path}"
    if hashlib.sha256(blob).hexdigest() != digest:
        return None, f"raw artifact digest mismatch: {raw_path}"
    return blob, None


def artifact_json(reference: Any, base: Path) -> tuple[dict[str, Any] | None, str | None]:
    blob, problem = artifact_bytes(reference, base, "raw-json")
    if problem:
        return None, problem
    try:
        value = json.loads(blob or b"")
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None, "raw JSON artifact is malformed"
    if not isinstance(value, dict):
        return None, "raw JSON artifact root is not an object"
    return value, None


def at(value: Mapping[str, Any], dotted: str) -> Any:
    current: Any = value
    for component in dotted.split("."):
        if not isinstance(current, Mapping) or component not in current:
            raise KeyError(dotted)
        current = current[component]
    return current


def compare(actual: Any, predicate: Mapping[str, Any], raw: Mapping[str, Any], total: int) -> bool:
    op = predicate.get("op")
    if op == "nonempty":
        return bool(actual)
    if op == "gt-half-denominator":
        return isinstance(actual, (int, float)) and actual * 2 > total
    expected = predicate.get("value")
    if "value_field" in predicate:
        expected = at(raw, str(predicate["value_field"]))
    if op == "eq":
        return actual == expected
    if op == "ne":
        return actual != expected
    if op == "gt":
        return isinstance(actual, (int, float)) and actual > expected
    raise ContractError(f"unsupported predicate operation {op!r}")


def manifest_problem(
    contract: Mapping[str, Any], criterion: Mapping[str, Any], item: Mapping[str, Any], subject: Subject
) -> str | None:
    if item.get("subject") != subject.as_dict():
        return "artifact manifest subject does not match the exact parent/Hermit/Reverie tuple"
    producer = criterion["producer"]
    expected = {
        "runner": producer["runner"],
        "runner_identity": contract["runners"][producer["runner"]]["identity"],
        "signer": producer["signer"],
        "signer_identity": contract["signers"][producer["signer"]]["identity"],
    }
    if item.get("authority") != expected:
        return "artifact manifest has no matching named runner/signer authority"
    return None


def producer_problem(
    criterion: Mapping[str, Any], raw: Mapping[str, Any], subject: Subject
) -> str | None:
    if raw.get("producer") != criterion["producer"]:
        return "raw artifact runner/signer identity does not match"
    if raw.get("subject") != subject.as_dict():
        return "raw artifact subject does not match the exact parent/Hermit/Reverie tuple"
    return None


def generic(
    criterion: Mapping[str, Any], item: Mapping[str, Any], base: Path, subject: Subject
) -> tuple[str, str, Mapping[str, Any]]:
    raw, problem = artifact_json(item.get("artifact"), base)
    if problem or raw is None:
        return "NO_RESULT", problem or "raw evidence is absent", {}
    if problem := producer_problem(criterion, raw, subject):
        return "NO_RESULT", problem, raw
    denominator = criterion["denominator"]
    try:
        total = at(raw, denominator["field"])
    except KeyError:
        return "NO_RESULT", "denominator field is absent", raw
    if type(total) is not int or total < int(denominator.get("minimum", 1)):
        return "NO_RESULT", "denominator is not a qualifying positive integer", raw
    for field_name in criterion["fields"]:
        try:
            at(raw, field_name)
        except KeyError:
            return "NO_RESULT", f"required field is absent: {field_name}", raw
    for forbidden in criterion["forbidden"]:
        try:
            if compare(at(raw, forbidden["field"]), forbidden, raw, total):
                return "FAIL", str(forbidden.get("reason") or "forbidden state observed"), raw
        except KeyError:
            return "NO_RESULT", "forbidden-state field is absent", raw
    threshold = criterion["threshold"]
    try:
        passed = compare(at(raw, threshold["field"]), threshold, raw, total)
    except KeyError:
        return "NO_RESULT", "threshold field is absent", raw
    return ("PASS", f"threshold satisfied over {total} {denominator['unit']}", raw) if passed else ("FAIL", "threshold not met", raw)


def manual_validate(
    criterion: Mapping[str, Any], item: Mapping[str, Any], base: Path, subject: Subject
) -> tuple[str, str, Mapping[str, Any]]:
    if item.get("producer") != criterion["producer"]:
        return "NO_RESULT", "manual runner/signer identity is absent", {}
    _, problem = artifact_bytes(item.get("artifact"), base, "raw-log")
    if problem:
        return "NO_RESULT", problem, {}
    exit_bytes, problem = artifact_bytes(item.get("exit_artifact"), base, "raw-exit")
    if problem:
        return "NO_RESULT", problem, {}
    try:
        pipeline_exit = int((exit_bytes or b"").decode("ascii").strip())
    except ValueError:
        return "NO_RESULT", "captured pipeline exit is not an integer", {}
    result_bytes, problem = artifact_bytes(
        item.get("result_artifact"), base, "raw-json"
    )
    if problem or result_bytes is None:
        return "NO_RESULT", problem or "validation service result is absent", {}
    try:
        framework = service_result.read_framework_result_bytes(
            result_bytes,
            expected_commit=subject.hermit_sha,
        )
    except RuntimeError as error:
        return "NO_RESULT", str(error), {}
    observed = {
        "commit": framework.commit,
        "profile": framework.profile,
        "selection_mode": framework.selection_mode,
        "final_validate_status": framework.final_validate_status,
        "exit_code": framework.inner_exit,
        "pipeline_exit": pipeline_exit,
        "executed_nodes": framework.executed_nodes,
        "executed_tests": framework.executed_tests,
        "passed_tests": framework.passed_tests,
        "service_result_schema": framework.service_result_schema,
        "scorecard_writeback": framework.scorecard_writeback,
        "result_source": framework.result_source,
    }
    if framework.service_result_schema != service_result.SCHEMA_VERSION:
        return (
            "NO_RESULT",
            "historical validation service result is readable but has no current authority",
            observed,
        )
    if framework.profile != "full":
        return "NO_RESULT", "validation service result profile is not full", observed
    if framework.selection_mode != "full":
        return "NO_RESULT", "validation service result selection is not full", observed
    if pipeline_exit != framework.inner_exit:
        return (
            "NO_RESULT",
            f"captured pipeline exit {pipeline_exit} disagrees with validation service result exit {framework.inner_exit}",
            observed,
        )
    final_status = framework.final_validate_status
    if final_status == "COULD_NOT_RUN":
        return "NO_RESULT", "validate reported COULD_NOT_RUN", observed
    if final_status == "FAILED":
        return "FAIL", "validate reported FAILED", observed
    if framework.scorecard_writeback is None:
        return "NO_RESULT", "scorecard write-back outcome is absent", observed
    if framework.scorecard_writeback.get("status") == "failed":
        return (
            "NO_RESULT",
            "validation passed but scorecard write-back failed: "
            f"{framework.scorecard_writeback['error']}",
            observed,
        )
    if framework.executed_nodes is None or framework.executed_nodes <= 0:
        return "NO_RESULT", "validation service result records zero executed nodes", observed
    if framework.executed_tests is None or framework.executed_tests <= 0:
        return "NO_RESULT", "validation service result records zero executed tests", observed
    if framework.passed_tests != framework.executed_tests:
        return "NO_RESULT", "validation service result lacks an exact passing count", observed
    return (
        "PASS",
        f"exact-RC manual validation executed {framework.executed_tests} tests, all {framework.passed_tests} passed, across {framework.executed_nodes} nodes",
        observed,
    )


def hosted(
    criterion: Mapping[str, Any], item: Mapping[str, Any], base: Path, subject: Subject
) -> tuple[str, str, Mapping[str, Any]]:
    raw, problem = artifact_json(item.get("artifact"), base)
    if problem or raw is None:
        return "NO_RESULT", problem or "raw hosted artifact is absent", {}
    if problem := producer_problem(criterion, raw, subject):
        return "NO_RESULT", problem, raw
    threshold = criterion["threshold"]
    expected_sha = subject.as_dict()[threshold["subject_sha"]]
    if raw.get("sha") != expected_sha:
        return "NO_RESULT", "raw named-job artifact is stale for the exact SHA", raw
    required = threshold["required_jobs"]
    jobs = raw.get("jobs")
    if not isinstance(jobs, list):
        return "NO_RESULT", "raw named-job artifact has no jobs", raw
    by_name = {job.get("name"): job for job in jobs if isinstance(job, Mapping)}
    if any(job.get("conclusion") == "failure" for job in jobs if isinstance(job, Mapping)):
        return "FAIL", "a required named job is red", raw
    missing = [name for name in required if name not in by_name]
    if missing:
        return "NO_RESULT", f"partial satisfying set: {len(required) - len(missing)}/{len(required)}", raw
    for name in required:
        job = by_name[name]
        if (
            job.get("head_sha") != expected_sha
            or job.get("status") != "completed"
            or job.get("conclusion") != "success"
            or type(job.get("run_id")) is not int
            or type(job.get("job_id")) is not int
        ):
            return "NO_RESULT", f"named job is not a dereferenced exact-head success: {name}", raw
    if raw.get("required_positive_count") != len(required) or raw.get("positive_count") != len(required):
        return "NO_RESULT", "named-job counts do not equal the complete satisfying set", raw
    return "PASS", f"complete raw named-job set {len(required)}/{len(required)}", raw


def scorecard(
    criterion: Mapping[str, Any], item: Mapping[str, Any], base: Path, subject: Subject
) -> tuple[str, str, Mapping[str, Any]]:
    raw, problem = artifact_json(item.get("artifact"), base)
    if problem or raw is None:
        return "NO_RESULT", problem or "raw scorecard artifact is absent", {}
    if problem := producer_problem(criterion, raw, subject):
        return "NO_RESULT", problem, raw
    cells = raw.get("fields", {}).get("cells") if isinstance(raw.get("fields"), Mapping) else None
    total = raw.get("denominator", {}).get("total") if isinstance(raw.get("denominator"), Mapping) else None
    if not isinstance(cells, list) or type(total) is not int or total <= 0 or total != len(cells):
        return "NO_RESULT", "scorecard denominator does not equal its cell population", raw
    for index, cell in enumerate(cells):
        if not isinstance(cell, Mapping) or cell.get("hermit_sha") != subject.hermit_sha or cell.get("reverie_sha") != subject.reverie_sha:
            return "NO_RESULT", f"scorecard cell {index} is stale", raw
        if type(cell.get("executed_tests")) is not int or cell["executed_tests"] <= 0:
            return "NO_RESULT", f"scorecard cell {index} executed zero tests", raw
        info = cell.get("info_records")
        if cell.get("stdout_parity") is not True or not isinstance(info, Mapping) or any(type(info.get(side)) is not int or info[side] <= 0 for side in ("left", "right")):
            return "NO_RESULT", f"scorecard cell {index} lacks stdout/INFO evidence", raw
        if cell.get("size_class") == "SHORT":
            if cell.get("comparison_tier") != "full-stdout-info-stack-heap" or cell.get("stack_parity") is not True or cell.get("heap_parity") is not True:
                return "NO_RESULT", f"SHORT cell {index} lacks full stack/heap tier", raw
        elif cell.get("size_class") == "LARGE":
            receipt = cell.get("spot_check_receipt")
            if (
                cell.get("comparison_tier") != "stdout-info-stack-heap-spot-check"
                or not isinstance(receipt, Mapping)
                or receipt.get("state") != "CURRENT"
                or receipt.get("hermit_sha") != subject.hermit_sha
                or receipt.get("reverie_sha") != subject.reverie_sha
                or receipt.get("stack_parity") is not True
                or receipt.get("heap_parity") is not True
            ):
                return "NO_RESULT", f"LARGE cell {index} lacks current exact-pair stack/heap spot check", raw
        else:
            return "NO_RESULT", f"scorecard cell {index} has no declared size tier", raw
    return "PASS", f"all {total}/{total} cells satisfy their recorded tiers", raw


def evaluate(
    contract: Mapping[str, Any], evidence: Mapping[str, Any], subject: Subject, repo_root: Path, evidence_dir: Path
) -> Report:
    report = Report(contract, subject, evidence.get("tested_rc_is_current_main") is True)
    report.deployment_errors.extend(validate_contract(contract, repo_root))
    if evidence.get("verified_hermit_base_sha") != VERIFIED_BASE_SHA:
        report.deployment_errors.append("evidence verified_hermit_base_sha is absent or wrong")
    if evidence.get("parent_contract_base_sha") != PARENT_CONTRACT_BASE_SHA:
        report.deployment_errors.append("evidence parent_contract_base_sha is absent or wrong")
    if report.deployment_errors:
        return report
    if evidence.get("tested_rc_sha") != subject.hermit_sha:
        for criterion in contract["criteria"]:
            report.results.append(Result(
                criterion,
                "NO_RESULT",
                "evidence tested_rc_sha does not equal the requested Hermit RC SHA; a rebase invalidates it",
            ))
        return report
    items = evidence.get("criteria") if isinstance(evidence.get("criteria"), Mapping) else {}
    for criterion in contract["criteria"]:
        item = items.get(criterion["id"])
        if not isinstance(item, Mapping):
            report.results.append(Result(criterion, "NO_RESULT", "criterion has no raw artifact"))
            continue
        if problem := manifest_problem(contract, criterion, item, subject):
            report.results.append(Result(criterion, "NO_RESULT", problem))
            continue
        evaluator = criterion["evaluator"]
        try:
            if evaluator == "manual_validate_result":
                verdict, reason, observed = manual_validate(criterion, item, evidence_dir, subject)
            elif evaluator == "hosted_set":
                verdict, reason, observed = hosted(criterion, item, evidence_dir, subject)
            elif evaluator == "scorecard_tiers":
                verdict, reason, observed = scorecard(criterion, item, evidence_dir, subject)
            else:
                verdict, reason, observed = generic(criterion, item, evidence_dir, subject)
        except (ContractError, KeyError, TypeError, ValueError) as error:
            verdict, reason, observed = "NO_RESULT", f"malformed raw evidence: {error}", {}
        report.results.append(Result(criterion, verdict, reason, observed))
    return report


def main(argv: Sequence[str] | None = None) -> int:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--contract", type=Path, default=root / "ai_docs/release-0.3-acceptance-contract.json")
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--parent-sha", required=True)
    parser.add_argument("--hermit-sha", required=True)
    parser.add_argument("--reverie-sha", required=True)
    parser.add_argument("--repo-root", type=Path, default=root)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        subject = Subject(args.parent_sha, args.hermit_sha, args.reverie_sha)
        contract = read_json(args.contract)
        evidence = read_json(args.evidence)
        report = evaluate(contract, evidence, subject, args.repo_root.resolve(), args.evidence.resolve().parent)
    except ContractError as error:
        print(f"DEPLOYMENT_DEFECT: {error}", file=sys.stderr)
        return DEPLOYMENT_DEFECT
    rendered = report.as_dict()
    if args.json:
        print(json.dumps(rendered, indent=2, sort_keys=True))
    else:
        print(f"{rendered['decision']}: PASS={rendered['counts']['PASS']}/{rendered['counts']['total']} FAIL={rendered['counts']['FAIL']} NO_RESULT={rendered['counts']['NO_RESULT']}")
        print(f"Hermit baseline: {rendered['hermit_baseline_status']}; tested RC: {rendered['tested_rc_status']}")
        print(f"parent contract base: {rendered['parent_contract_base_sha']}; parent: {rendered['parent_status']}")
        for result in report.results:
            print(f"{result.criterion['id']} {result.verdict}: {result.reason}")
    return report.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
