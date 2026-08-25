from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timezone
from pathlib import Path
from unittest import mock

REMEDIATION = Path(__file__).resolve().parents[1]
HISTORY = REMEDIATION.parents[0] / "history"
HERMIT = REMEDIATION.parents[1] / "hermit"
PRODUCER_REGISTRY = REMEDIATION.parents[0] / "validate" / "producer-definition.json"
sys.path.insert(0, str(REMEDIATION))
sys.path.insert(0, str(HISTORY))

import obligations
import land_and_arm
import protocol

SHA = "a" * 40
NEXT_SHA = "b" * 40
TREE = "c" * 40
REVERIE_LANDED_SHA = "025d37800d347c32711038bd0a3889e8e4774c2b"


class SharedTrackingRefFetchTests(unittest.TestCase):
    def test_fetch_target_uses_unique_ref_then_bounded_cas_helper(self) -> None:
        source = Path("/source")
        with mock.patch.object(
            protocol.fetch_ref, "fetch_remote_tracking_branch"
        ) as safe_fetch:
            self.assertEqual(protocol._fetch_target(source, "main"), "origin/main")
        safe_fetch.assert_called_once_with(
            source,
            remote="origin",
            branch="main",
            use_proxy=True,
            network_timeout_s=protocol.DEFAULT_NETWORK_TIMEOUT,
        )

    def test_fetch_target_preserves_fail_closed_refusal(self) -> None:
        with (
            mock.patch.object(
                protocol.fetch_ref,
                "fetch_remote_tracking_branch",
                side_effect=protocol.fetch_ref.FetchRefError("CAS still moving"),
            ),
            self.assertRaisesRegex(protocol.ProtocolError, "CAS still moving"),
        ):
            protocol._fetch_target(Path("/source"), "main")


def _producer_state(state: str) -> tuple[str, str | None]:
    return {
        "green": ("completed", "success"),
        "red": ("completed", "failure"),
        "pending": ("queued", None),
        "running": ("in_progress", None),
        "no_result": ("completed", "skipped"),
    }[state]


def hosted_evidence(
    repo: str,
    *,
    sha: str = SHA,
    job_states: tuple[str, ...] | None = None,
) -> list[dict]:
    policy = protocol.verification_policy_for_repo(repo)
    if job_states is None:
        job_states = ("green",) * len(policy["github"]["required_jobs"])
    runs: dict[tuple[str, str], dict] = {}
    for index, (required, state) in enumerate(
        zip(policy["github"]["required_jobs"], job_states, strict=True), 1
    ):
        workflow = (required["workflow_file"], required["workflow_name"])
        run = runs.setdefault(
            workflow,
            {
                "databaseId": 100 + len(runs),
                "headSha": sha,
                "workflowFile": workflow[0],
                "workflowName": workflow[1],
                "status": "completed",
                "conclusion": "success",
                "createdAt": "2026-08-05T00:00:00Z",
                "startedAt": "2026-08-05T00:00:01Z",
                "updatedAt": "2026-08-05T00:10:00Z",
                "url": f"https://github.invalid/runs/{100 + len(runs)}",
                "event": "push",
                "jobs": [],
            },
        )
        status, conclusion = _producer_state(state)
        run["jobs"].append(
            {
                "id": 1000 + index,
                "run_id": run["databaseId"],
                "head_sha": sha,
                "name": required["job_name"],
                "status": status,
                "conclusion": conclusion,
                "started_at": "2026-08-05T00:00:02Z",
                "completed_at": (
                    "2026-08-05T00:09:00Z" if status == "completed" else None
                ),
                "html_url": f"https://github.invalid/jobs/{1000 + index}",
            }
        )
        if status == "in_progress":
            run["status"] = "in_progress"
            run["conclusion"] = None
        elif status == "queued" and run["status"] != "in_progress":
            run["status"] = "queued"
            run["conclusion"] = None
    return list(runs.values())


def hosted_api_payloads(evidence: list[dict]) -> list[dict]:
    """Render the run and job pages returned by the GitHub API fixture."""
    runs = {
        "total_count": len(evidence),
        "workflow_runs": [
            {
                "id": run["databaseId"],
                "head_sha": run["headSha"],
                "path": run["workflowFile"],
                "name": run["workflowName"],
                "status": run["status"],
                "conclusion": run["conclusion"],
                "created_at": run["createdAt"],
                "run_started_at": run["startedAt"],
                "updated_at": run["updatedAt"],
                "html_url": run["url"],
                "event": run["event"],
            }
            for run in evidence
        ],
    }
    return [runs, *(
        {"total_count": len(run["jobs"]), "jobs": run["jobs"]}
        for run in evidence
    )]


def counted_receipt_report(sha: str = SHA) -> dict:
    finished_at = "2026-08-05T00:10:00Z"
    host = "test-host"
    slot = "test"
    log_file = "/durable/validate.log"
    report = {
        "schema_version": 1,
        "repo": "rrnewton/hermit",
        "sha": sha,
        "verdict": "VALIDATED",
        "exit_code": 0,
        "qualifying_count": 1,
        "disqualified_count": 0,
        "newest_qualifying": {
            "schema_version": 4,
            "repo": "rrnewton/hermit",
            "sha": sha,
            "commit": sha,
            "tree": TREE,
            "commit_anchored": True,
            "tree_dirty": False,
            "finished_at": finished_at,
            "host": host,
            "profile": "full",
            "selection_mode": "full",
            "result": "pass",
            "raw_result": "pass",
            "exit_code": 0,
            "checks": 2,
            "failures": 0,
            "gates_run": 2,
            "gates_expected": 2,
            "gates": [
                {"name": "fmt", "result": "pass", "exit_code": 0},
                {"name": "test", "result": "pass", "exit_code": 0},
            ],
            "executed_tests": 10,
            "filtered_tests": 3,
            "selected_tests": 10,
            "discovered_tests": 13,
            "count_derivation": protocol.LOCAL_RECEIPT_COUNT_DERIVATION,
            "coverage": None,
            "coverage_satisfied": None,
            "coverage_status": protocol.LOCAL_SCHEMA4_COVERAGE_STATUS,
            "coverage_basis": protocol.LOCAL_SCHEMA4_COVERAGE_BASIS,
            "real_seconds": 10.0,
            "user_seconds": 8.0,
            "sys_seconds": 2.0,
            "slot": slot,
            "log_file": log_file,
            "receipt_identity": {
                "digest_algorithm": "sha256",
                "canonicalization": protocol.LOCAL_RECEIPT_CANONICALIZATION,
                "digest": "d" * 64,
                "tuple": {
                    "repo": "rrnewton/hermit",
                    "sha": sha,
                    "tree": TREE,
                    "finished_at": finished_at,
                    "host": host,
                    "slot": slot,
                    "log_file": log_file,
                },
            },
        },
        "ledger": "/durable/validate-run-ledger.jsonl",
    }
    report["qualifying_receipts"] = [
        json.loads(json.dumps(report["newest_qualifying"]))
    ]
    return report


def failed_receipt_report(sha: str = SHA) -> dict:
    return {
        "schema_version": 1,
        "repo": "rrnewton/hermit",
        "sha": sha,
        "verdict": "FAILED",
        "exit_code": 3,
        "qualifying_count": 0,
        "disqualified_count": 1,
        "failed_record_count": 1,
        "withheld_nonpass_record_count": 0,
        "newest_qualifying": None,
        "qualifying_receipts": [],
        "ledger": "/durable/validate-run-ledger.jsonl",
    }


def ledger_receipt(sha: str = SHA) -> dict:
    return {
        "schema_version": 4,
        "started_at": "2026-08-05T00:00:00Z",
        "finished_at": "2026-08-05T00:10:00Z",
        "host": "test-host",
        "slot": "test",
        "profile": "full",
        "selection_mode": "full",
        "commit": sha,
        "tree": TREE,
        "commit_anchored": True,
        "tree_dirty": False,
        "result": "pass",
        "raw_result": "pass",
        "exit_code": 0,
        "executed_tests": 10,
        "filtered_tests": 3,
        "checks": 2,
        "gates_run": 2,
        "gates_expected": 2,
        "failures": 0,
        "log_file": "/durable/validate.log",
        "gates": [
            {"name": "fmt", "result": "pass", "exit_code": 0},
            {"name": "test", "result": "pass", "exit_code": 0},
        ],
    }


def verified_local_receipt(sha: str = SHA) -> dict:
    report = counted_receipt_report(sha)
    canonical = json.dumps(report, sort_keys=True, separators=(",", ":")).encode()
    return {
        "state": "verified",
        "authority": "ci-hub-validate-status",
        "repo": "rrnewton/hermit",
        "command": [
            str(protocol.LOCAL_RECEIPT_AUTHORITY),
            "validate-status",
            "--sha",
            sha,
            "--repo",
            "rrnewton/hermit",
            "--json",
        ],
        "checked_at": "2026-08-05T00:11:00Z",
        "returncode": 0,
        "report": report,
        "report_sha256": hashlib.sha256(canonical).hexdigest(),
        "reason": None,
    }


def failed_local_receipt(sha: str = SHA) -> dict:
    report = failed_receipt_report(sha)
    canonical = json.dumps(report, sort_keys=True, separators=(",", ":")).encode()
    return {
        "state": "failed",
        "authority": "ci-hub-validate-status",
        "repo": "rrnewton/hermit",
        "command": [
            str(protocol.LOCAL_RECEIPT_AUTHORITY),
            "validate-status",
            "--sha",
            sha,
            "--repo",
            "rrnewton/hermit",
            "--json",
        ],
        "checked_at": "2026-08-05T00:11:00Z",
        "returncode": 3,
        "report": report,
        "report_sha256": hashlib.sha256(canonical).hexdigest(),
        "reason": "canonical verifier returned FAILED/3",
    }


def local_green(sha: str = SHA) -> dict:
    return {
        "state": "green",
        "exit_code": 0,
        "launch_token": "local-launch-token",
        "registered_at": "2026-08-05T00:09:00Z",
        "started_at": "2026-08-05T00:09:01Z",
        "pid": protocol.os.getpid(),
        "finished_at": "2026-08-05T00:11:00Z",
        "receipt_verification": verified_local_receipt(sha),
    }


def durable_launch_patch() -> dict:
    return {
        "launch": {
            "state": "armed",
            "token": "test-launch-token",
            "launcher_pid": None,
            "armed_at": "2026-08-05T00:12:00Z",
        },
        "local": protocol._local_policy_skip_patch("rrnewton/reverie"),
        "watcher": {
            "state": "running",
            "pid": protocol.os.getpid(),
            "started_at": "2026-08-05T00:10:00Z",
            "finished_at": None,
        },
    }


class ProtocolTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.store = self.root / "obligations.jsonl"
        current_local_receipt_patch = mock.patch.object(
            protocol,
            "_dereference_current_local_receipt",
            side_effect=lambda _repo, sha: (True, verified_local_receipt(sha)),
        )
        self.current_local_receipt = current_local_receipt_patch.start()
        self.addCleanup(current_local_receipt_patch.stop)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def canonical_receipt_no_result(self) -> None:
        self.current_local_receipt.side_effect = None
        self.current_local_receipt.return_value = (
            False,
            {
                "state": "refused",
                "authority": "ci-hub-validate-status",
                "repo": "rrnewton/hermit",
                "reason": "canonical verifier reported no qualifying counted receipt",
            },
        )

    def typed_skip_report(self) -> dict:
        report = counted_receipt_report()
        row = report["newest_qualifying"]
        row["gates_expected"] = 3
        row["skipped_nodes"] = 1
        row["intentional_skipped_nodes"] = [
            {
                "name": "privileged-e2e.manifest_applications",
                "reason": "empty-manifest-bucket",
            }
        ]
        row["dependency_skipped_nodes"] = []
        row["unaccounted_nodes"] = []
        report["qualifying_receipts"] = [json.loads(json.dumps(row))]
        return report

    def test_typed_empty_bucket_is_accounted_but_not_an_executed_gate(self) -> None:
        report = self.typed_skip_report()
        problem = protocol._local_receipt_problem(
            report, repo="rrnewton/hermit", sha=SHA, returncode=0
        )
        self.assertIsNone(problem)
        row = report["newest_qualifying"]
        self.assertEqual(row["gates_run"], 2)
        self.assertEqual(len(row["gates"]), 2)

    def test_unknown_intentional_skip_reason_is_refused(self) -> None:
        report = self.typed_skip_report()
        report["newest_qualifying"]["intentional_skipped_nodes"][0][
            "reason"
        ] = "looks-empty"
        report["qualifying_receipts"] = [
            json.loads(json.dumps(report["newest_qualifying"]))
        ]
        problem = protocol._local_receipt_problem(
            report, repo="rrnewton/hermit", sha=SHA, returncode=0
        )
        self.assertEqual(problem, "qualifying receipt has inconsistent gate coverage")

    def test_dependency_skip_cannot_hide_behind_intentional_skip(self) -> None:
        report = self.typed_skip_report()
        report["newest_qualifying"]["dependency_skipped_nodes"] = ["lost.node"]
        report["qualifying_receipts"] = [
            json.loads(json.dumps(report["newest_qualifying"]))
        ]
        problem = protocol._local_receipt_problem(
            report, repo="rrnewton/hermit", sha=SHA, returncode=0
        )
        self.assertEqual(problem, "qualifying receipt has inconsistent gate coverage")

    def repo_source(self, repo: str, name: str = "source-repo") -> Path:
        source = self.root / name
        if not source.exists():
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(source),
                    "remote",
                    "add",
                    "origin",
                    f"https://github.com/{repo}.git",
                ],
                check=True,
            )
        return source

    def create(self) -> dict:
        return obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=SHA,
            land_mode="speculative",
            verification_policy=protocol.verification_policy_for_repo(
                "rrnewton/hermit"
            ),
            actor="test",
            obligation_id="test-obligation",
            path=self.store,
        )

    def revert_snapshot(
        self,
        *,
        candidate_sha: str = NEXT_SHA,
        main_sha: str = SHA,
        issuer: str = protocol.REVERT_AUTHORIZATION_OWNER,
        association: str = "OWNER",
        expires_at: str = "2099-01-01T00:00:00Z",
        nonce: str = "test-revert-nonce-0001",
    ) -> dict:
        return {
            "authorization": {
                "repo": "rrnewton/hermit",
                "obligation_id": "test-obligation",
                "failed_land_sha": SHA,
                "main_sha": main_sha,
                "candidate_sha": candidate_sha,
                "nonce": nonce,
                "expires_at": expires_at,
                "authorized_by": protocol.REVERT_AUTHORIZATION_OWNER,
                "reason": "owner reviewed the exact revert candidate",
            },
            "issuer": {
                "login": issuer,
                "author_association": association,
                "comment_id": 42,
                "html_url": "https://example.invalid/comment/42",
            },
            "observed": {
                "main_sha": SHA,
                "candidate_sha": NEXT_SHA,
                "candidate_parent_sha": SHA,
                "candidate_tree_sha": TREE,
                "failed_parent_tree_sha": TREE,
            },
        }

    def resolve_args(self, *, source: Path | None = None) -> argparse.Namespace:
        return argparse.Namespace(
            id="test-obligation",
            kind="fix-forward",
            ref=NEXT_SHA,
            started_at=None,
            source=source,
            store=self.store,
        )

    def test_rebase_merged_pr_resolves_to_replayed_main_sha(self) -> None:
        source = self.root / "source"
        source.mkdir()
        head = "c" * 40
        replay = "d" * 40

        def run(command, **_kwargs):
            command = tuple(command)
            if command[:2] == ("with-proxy", "git"):
                return subprocess.CompletedProcess(command, 0, "", "")
            if command[:3] == ("with-proxy", "gh", "pr"):
                payload = {
                    "state": "MERGED",
                    "headRefOid": head,
                    "mergeCommit": {"oid": replay},
                }
                return subprocess.CompletedProcess(command, 0, json.dumps(payload), "")
            if "--is-ancestor" in command:
                self.assertEqual(command[-2:], (replay, "origin/main"))
                return subprocess.CompletedProcess(command, 0, "", "")
            self.fail(f"unexpected command: {command}")

        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(protocol, "_run", side_effect=run),
        ):
            landed = protocol.resolve_landed_sha(
                source,
                "pre-rebase-head-not-fetched",
                repo="rrnewton/hermit",
                pr=1219,
            )
        self.assertEqual(landed, replay)

    def test_raw_rebase_head_failure_explains_pr_aware_check(self) -> None:
        source = self.root / "source"
        source.mkdir()
        head = "c" * 40

        def run(command, **_kwargs):
            command = tuple(command)
            if command[:2] == ("with-proxy", "git"):
                return subprocess.CompletedProcess(command, 0, "", "")
            if "rev-parse" in command:
                return subprocess.CompletedProcess(command, 0, head + "\n", "")
            if "--is-ancestor" in command:
                return subprocess.CompletedProcess(command, 1, "", "")
            self.fail(f"unexpected command: {command}")

        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(protocol, "_run", side_effect=run),
        ):
            with self.assertRaisesRegex(protocol.ProtocolError, "pass --pr"):
                protocol.resolve_landed_sha(source, head)

    def test_rebase_replay_sha_must_still_be_on_main(self) -> None:
        source = self.root / "source"
        source.mkdir()
        head = "c" * 40
        replay = "d" * 40

        def run(command, **_kwargs):
            command = tuple(command)
            if command[:2] == ("with-proxy", "git"):
                return subprocess.CompletedProcess(command, 0, "", "")
            if command[:3] == ("with-proxy", "gh", "pr"):
                payload = {
                    "state": "MERGED",
                    "headRefOid": head,
                    "mergeCommit": {"oid": replay},
                }
                return subprocess.CompletedProcess(command, 0, json.dumps(payload), "")
            if "--is-ancestor" in command:
                return subprocess.CompletedProcess(command, 1, "", "")
            self.fail(f"unexpected command: {command}")

        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(protocol, "_run", side_effect=run),
        ):
            with self.assertRaisesRegex(protocol.ProtocolError, "orphaned"):
                protocol.resolve_landed_sha(
                    source, head, repo="rrnewton/hermit", pr=1219
                )

    def test_verify_landing_pr_reports_replay_ancestry(self) -> None:
        source = self.repo_source("rrnewton/hermit")
        replay = "d" * 40
        args = argparse.Namespace(
            source=source,
            reference="1219",
            target="main",
            repo="rrnewton/hermit",
            json=True,
            item=None,
            claimed_oid=None,
        )
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol,
                "_query_pr_landing",
                return_value=("MERGED", "c" * 40, replay),
            ),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=True),
            redirect_stdout(io.StringIO()) as stdout,
        ):
            rc = protocol.verify_landing(args)
        payload = json.loads(stdout.getvalue())
        self.assertEqual(rc, 0)
        self.assertEqual(payload["state"], "landed")
        self.assertEqual(payload["rc"], 0)
        self.assertEqual(payload["resolved_sha"], replay)

    def test_verify_landing_sha_reports_not_landed(self) -> None:
        source = self.repo_source("rrnewton/hermit")
        args = argparse.Namespace(
            source=source,
            reference=SHA,
            target="main",
            repo="rrnewton/hermit",
            json=True,
            item=None,
            claimed_oid=None,
        )
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(protocol, "_resolve_raw_sha", return_value=SHA),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=False),
            redirect_stdout(io.StringIO()) as stdout,
        ):
            rc = protocol.verify_landing(args)
        payload = json.loads(stdout.getvalue())
        self.assertEqual(rc, 1)
        self.assertEqual(payload["state"], "not-landed")
        self.assertEqual(payload["rc"], 1)
        self.assertEqual(payload["ancestry"], "not-ancestor")

    def test_verify_landing_parent_sha_brackets_ancestry(self) -> None:
        source = self.repo_source(protocol.PARENT_REPO, "parent-landing-source")
        for is_ancestor, expected_rc, expected_state in (
            (True, 0, "landed"),
            (False, 1, "not-landed"),
        ):
            with self.subTest(is_ancestor=is_ancestor):
                args = argparse.Namespace(
                    source=source,
                    reference=SHA,
                    target="main",
                    repo=protocol.PARENT_REPO,
                    json=True,
                    item=None,
                    claimed_oid=None,
                )
                with (
                    mock.patch.object(
                        protocol, "_fetch_target", return_value="origin/main"
                    ),
                    mock.patch.object(protocol, "_resolve_raw_sha", return_value=SHA),
                    mock.patch.object(
                        protocol,
                        "_is_target_ancestor",
                        return_value=is_ancestor,
                    ),
                    redirect_stdout(io.StringIO()) as stdout,
                ):
                    rc = protocol.verify_landing(args)
                payload = json.loads(stdout.getvalue())
                self.assertEqual(expected_rc, rc)
                self.assertEqual(expected_state, payload["state"])
                self.assertEqual(SHA, payload["resolved_sha"])

    def test_verify_landing_pr_without_merge_commit_is_unverifiable(self) -> None:
        source = self.repo_source("rrnewton/hermit")
        args = argparse.Namespace(
            source=source,
            reference="1558",
            target="main",
            repo="rrnewton/hermit",
            json=True,
            item=None,
            claimed_oid=None,
        )
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol,
                "_query_pr_landing",
                return_value=("OPEN", "e" * 40, ""),
            ),
            redirect_stdout(io.StringIO()) as stdout,
        ):
            rc = protocol.verify_landing(args)
        payload = json.loads(stdout.getvalue())
        self.assertEqual(rc, 2)
        self.assertEqual(payload["state"], "unverifiable")
        self.assertEqual(payload["rc"], 2)
        self.assertEqual(payload["reason"], "no mergeCommit.oid")

    def test_verify_landing_expands_abbreviated_direct_commit(self) -> None:
        source = self.repo_source("rrnewton/hermit")
        args = argparse.Namespace(
            source=source,
            reference=SHA[:7],
            target="main",
            repo="rrnewton/hermit",
            json=True,
            item="direct main change",
            claimed_oid=None,
        )
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(protocol, "_resolve_raw_sha", return_value=SHA),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=True),
            redirect_stdout(io.StringIO()) as stdout,
        ):
            rc = protocol.verify_landing(args)
        payload = json.loads(stdout.getvalue())
        self.assertEqual(rc, 0)
        self.assertEqual(payload["claimed_oid"], SHA[:7])
        self.assertEqual(payload["full_oid"], SHA)
        self.assertTrue(payload["resolves"])
        self.assertTrue(payload["change_present_on_main"])
        self.assertEqual(payload["claimed_ancestry_rc"], 0)

    def test_verify_landing_separates_dead_head_from_live_rebased_change(self) -> None:
        source = self.repo_source("rrnewton/hermit")
        head = "abedbe29" + "c" * 32
        replay = "d" * 40
        args = argparse.Namespace(
            source=source,
            reference="1592",
            target="main",
            repo="rrnewton/hermit",
            json=True,
            item="PR #1592",
            claimed_oid="abedbe29",
        )

        def is_ancestor(_source, sha, _target):
            return sha == replay

        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol,
                "_query_pr_landing",
                return_value=("MERGED", head, replay),
            ),
            mock.patch.object(
                protocol,
                "_resolve_claimed_oid",
                return_value=(head, True, True),
            ),
            mock.patch.object(protocol, "_is_target_ancestor", side_effect=is_ancestor),
            redirect_stdout(io.StringIO()) as stdout,
        ):
            rc = protocol.verify_landing(args)
        payload = json.loads(stdout.getvalue())
        self.assertEqual(rc, 0, "the PR's rebased change is landed")
        self.assertEqual(payload["state"], "landed")
        self.assertEqual(payload["claimed_oid"], "abedbe29")
        self.assertEqual(payload["full_oid"], head)
        self.assertTrue(payload["resolves"])
        self.assertTrue(payload["change_present_on_main"])
        self.assertEqual(payload["claimed_ancestry_rc"], 1)
        self.assertEqual(payload["merge_commit_oid"], replay)

    def transition(self, patch: dict) -> dict:
        patch = json.loads(json.dumps(patch))
        if (patch.get("local") or {}).get("state") == "green":
            patch["local"] = {**local_green(), **patch["local"]}
        return obligations.transition(
            "test-obligation", "test-transition", patch, self.store
        )

    def require_remediation(self, kind: str = "fix-forward") -> dict:
        return self.transition(
            {
                "overall_state": "remediation_required",
                "recommendation": {"action": kind},
                "remediation": {"state": "triggered", "kind": kind},
            }
        )

    def test_github_parser_requires_exact_sha_and_workflow(self) -> None:
        policy = protocol.verification_policy_for_repo("rrnewton/hermit")
        payload = {
            "total_count": 3,
            "workflow_runs": [
                {
                    "id": 1,
                    "head_sha": SHA,
                    "path": protocol.DEFAULT_WORKFLOW_FILE,
                    "name": protocol.DEFAULT_WORKFLOW,
                    "created_at": "2026-08-03T01:00:00Z",
                },
                {
                    "id": 2,
                    "head_sha": SHA,
                    "path": protocol.PRIVILEGED_WORKFLOW_FILE,
                    "name": protocol.PRIVILEGED_WORKFLOW,
                    "created_at": "2026-08-03T02:00:00Z",
                },
                {
                    "id": 3,
                    "head_sha": SHA,
                    "path": ".github/workflows/docs.yml",
                    "name": "Docs",
                    "created_at": "2026-08-03T03:00:00Z",
                },
            ],
        }
        runs = protocol._parse_github_runs(json.dumps(payload), SHA, policy)
        self.assertEqual([run["databaseId"] for run in runs], [1])

        payload["workflow_runs"][0]["head_sha"] = NEXT_SHA
        with self.assertRaisesRegex(protocol.ProtocolError, "expected exact SHA"):
            protocol._parse_github_runs(json.dumps(payload), SHA, policy)
        payload["workflow_runs"][0]["head_sha"] = SHA
        payload["workflow_runs"][0]["name"] = "Wrong portable proxy"
        with self.assertRaisesRegex(protocol.ProtocolError, "expected"):
            protocol._parse_github_runs(json.dumps(payload), SHA, policy)

    def test_repo_policy_routes_exact_workflow_queries(self) -> None:
        for repo in ("rrnewton/hermit", "rrnewton/reverie", protocol.PARENT_REPO):
            evidence = hosted_evidence(repo)
            raw_runs = {
                "total_count": len(evidence),
                "workflow_runs": [
                    {
                        "id": run["databaseId"],
                        "head_sha": run["headSha"],
                        "path": run["workflowFile"],
                        "name": run["workflowName"],
                        "status": run["status"],
                        "conclusion": run["conclusion"],
                        "created_at": run["createdAt"],
                        "run_started_at": run["startedAt"],
                        "updated_at": run["updatedAt"],
                        "html_url": run["url"],
                        "event": run["event"],
                    }
                    for run in evidence
                ],
            }
            responses = [raw_runs]
            responses.extend(
                {"total_count": len(run["jobs"]), "jobs": run["jobs"]}
                for run in evidence
            )
            completed = [
                subprocess.CompletedProcess([], 0, json.dumps(response), "")
                for response in responses
            ]
            with (
                self.subTest(repo=repo),
                mock.patch.object(protocol, "_run", side_effect=completed) as run,
            ):
                runs = protocol.github_runs(repo, SHA)
            first_command = run.call_args_list[0].args[0]
            self.assertIn(
                f"repos/{repo}/actions/runs?head_sha={SHA}", first_command[-1]
            )
            self.assertEqual(
                {job["name"] for observed in runs for job in observed["jobs"]},
                {
                    required["job_name"]
                    for required in protocol.verification_policy_for_repo(repo)[
                        "github"
                    ]["required_jobs"]
                },
            )

    def test_watch_deadline_rebounds_each_github_call(self) -> None:
        """Every serial API call gets only the current remaining allowance."""
        evidence = hosted_evidence("rrnewton/hermit")
        raw_runs = {
            "total_count": len(evidence),
            "workflow_runs": [
                {
                    "id": run["databaseId"],
                    "head_sha": run["headSha"],
                    "path": run["workflowFile"],
                    "name": run["workflowName"],
                    "status": run["status"],
                    "conclusion": run["conclusion"],
                    "created_at": run["createdAt"],
                    "run_started_at": run["startedAt"],
                    "updated_at": run["updatedAt"],
                    "html_url": run["url"],
                    "event": run["event"],
                }
                for run in evidence
            ],
        }
        responses = [raw_runs]
        responses.extend(
            {"total_count": len(run["jobs"]), "jobs": run["jobs"]}
            for run in evidence
        )
        clock = {"now": 100.0}
        observed_timeouts: list[float] = []

        def fake_run(command, *, check, timeout):
            del command, check
            observed_timeouts.append(timeout)
            clock["now"] += 3.0
            return subprocess.CompletedProcess(
                [], 0, json.dumps(responses.pop(0)), ""
            )

        with (
            mock.patch.object(protocol.time, "monotonic", lambda: clock["now"]),
            mock.patch.object(protocol, "_run", side_effect=fake_run),
        ):
            protocol.github_runs("rrnewton/hermit", SHA, deadline=120.0)
        self.assertGreaterEqual(len(observed_timeouts), 2)
        self.assertEqual(observed_timeouts[0], 18.0)
        self.assertEqual(observed_timeouts[1], 15.0)
        self.assertTrue(all(timeout < 20.0 for timeout in observed_timeouts))

    def test_github_main_sha_keeps_default_timeout_without_watch_deadline(
        self,
    ) -> None:
        with mock.patch.object(
            protocol,
            "_run",
            return_value=subprocess.CompletedProcess([], 0, f"{SHA}\n", ""),
        ) as run:
            self.assertEqual(protocol.github_main_sha("rrnewton/reverie"), SHA)
        self.assertEqual(run.call_args.kwargs["timeout"], protocol.DEFAULT_NETWORK_TIMEOUT)

    def test_terminal_red_watch_bounds_main_sha_and_reports_no_result(self) -> None:
        """A slow main-SHA query cannot consume the one-shot reporting tail."""
        repo = "rrnewton/reverie"
        obligation_id = "bounded-main-sha"
        obligations.create_obligation(
            repo=repo,
            landed_sha=REVERIE_LANDED_SHA,
            land_mode="speculative",
            verification_policy=protocol.verification_policy_for_repo(repo),
            actor="test",
            obligation_id=obligation_id,
            path=self.store,
        )
        evidence = hosted_evidence(
            repo,
            sha=REVERIE_LANDED_SHA,
            job_states=("red", "red"),
        )
        responses = hosted_api_payloads(evidence)
        clock = {"now": 0.0}
        observed: list[tuple[tuple[str, ...], float]] = []

        def fake_run(command, *, check, timeout):
            del check
            command = tuple(command)
            observed.append((command, timeout))
            if f"repos/{repo}/commits/main" in command:
                clock["now"] += timeout
                try:
                    raise subprocess.TimeoutExpired(command, timeout)
                except subprocess.TimeoutExpired as error:
                    raise protocol.ProtocolError("planted main query timeout") from error
            clock["now"] += 3.0
            return subprocess.CompletedProcess(
                command, 0, json.dumps(responses.pop(0)), ""
            )

        output = io.StringIO()
        with (
            mock.patch.object(protocol.time, "monotonic", lambda: clock["now"]),
            mock.patch.object(protocol, "_run", side_effect=fake_run),
            redirect_stdout(output),
        ):
            rc = protocol.watch(
                store_path=self.store,
                obligation_id=obligation_id,
                once=True,
                poll_seconds=1,
                budget_secs=25.0,
            )
        current = obligations.get_record(obligation_id, self.store)
        self.assertEqual(rc, protocol.WATCH_EXIT_NO_RESULT)
        self.assertEqual(current["github"]["state"], "red")
        self.assertEqual(current["overall_state"], "open")
        self.assertIn("watch_verdict=NO-RESULT", output.getvalue())
        self.assertNotIn("watch_status=complete", output.getvalue())
        self.assertEqual(len(observed), 3)
        self.assertIn(f"repos/{repo}/actions/runs", observed[0][0][3])
        self.assertIn("/jobs", observed[1][0][3])
        self.assertIn(f"repos/{repo}/commits/main", observed[2][0])
        self.assertEqual([timeout for _, timeout in observed], [23.0, 20.0, 17.0])
        self.assertEqual(clock["now"], 23.0)

    def test_terminal_red_poll_with_allowance_returns_normal_result(self) -> None:
        repo = "rrnewton/reverie"
        obligation_id = "main-sha-with-allowance"
        store = self.root / "main-sha-with-allowance.jsonl"
        obligations.create_obligation(
            repo=repo,
            landed_sha=REVERIE_LANDED_SHA,
            land_mode="speculative",
            verification_policy=protocol.verification_policy_for_repo(repo),
            actor="test",
            obligation_id=obligation_id,
            path=store,
        )
        evidence = hosted_evidence(
            repo,
            sha=REVERIE_LANDED_SHA,
            job_states=("red", "red"),
        )
        responses = hosted_api_payloads(evidence)
        clock = {"now": 0.0}
        observed: list[tuple[tuple[str, ...], float]] = []

        def fake_run(command, *, check, timeout):
            del check
            command = tuple(command)
            observed.append((command, timeout))
            clock["now"] += 1.0
            stdout = (
                f"{REVERIE_LANDED_SHA}\n"
                if f"repos/{repo}/commits/main" in command
                else json.dumps(responses.pop(0))
            )
            return subprocess.CompletedProcess(command, 0, stdout, "")

        with (
            mock.patch.object(protocol.time, "monotonic", lambda: clock["now"]),
            mock.patch.object(protocol, "_run", side_effect=fake_run),
        ):
            record = protocol.poll_obligation(
                obligation_id,
                store,
                deadline=25.0,
            )
        self.assertEqual(record["overall_state"], "remediation_required")
        self.assertEqual(record["remediation"]["state"], "awaiting_owner")
        self.assertEqual(len(observed), 3)
        self.assertIn(f"repos/{repo}/commits/main", observed[2][0])
        self.assertEqual([timeout for _, timeout in observed], [23.0, 22.0, 21.0])

    def test_hosted_green_fast_path_passes_watch_deadline_to_evaluation(self) -> None:
        deadline = 25.0
        record = {
            "obligation_id": "hosted-green-fast-path",
            "repo": "rrnewton/reverie",
            "landed_sha": REVERIE_LANDED_SHA,
            "overall_state": "open",
            "local": {"state": "no_result"},
            "github": {"state": "green"},
        }
        satisfied = {**record, "overall_state": "satisfied"}
        with (
            mock.patch.object(protocol.obligations, "get_record", return_value=record),
            mock.patch.object(protocol, "_verification_in_flight", return_value=False),
            mock.patch.object(
                protocol, "_verification_state_needs_reconcile", return_value=False
            ),
            mock.patch.object(
                protocol, "evaluate_obligation", return_value=satisfied
            ) as evaluate,
        ):
            self.assertEqual(
                protocol.poll_obligation(
                    record["obligation_id"], self.store, deadline=deadline
                ),
                satisfied,
            )
        evaluate.assert_called_once_with(
            record["obligation_id"],
            store_path=self.store,
            deadline=deadline,
        )

    def test_watch_deadline_refuses_call_without_completion_reserve(self) -> None:
        with (
            mock.patch.object(protocol.time, "monotonic", return_value=119.0),
            mock.patch.object(protocol, "_run") as run,
            self.assertRaisesRegex(
                protocol.WatchBudgetExhausted, "before the next GitHub API call"
            ),
        ):
            protocol.github_runs("rrnewton/hermit", SHA, deadline=120.0)
        run.assert_not_called()

    def test_inner_subprocess_timeout_becomes_watch_no_result(self) -> None:
        def planted_timeout(*args, **kwargs):
            del args, kwargs
            try:
                raise subprocess.TimeoutExpired(["gh", "api"], 18.0)
            except subprocess.TimeoutExpired as error:
                raise protocol.ProtocolError("planted command timeout") from error

        with (
            mock.patch.object(protocol.time, "monotonic", return_value=100.0),
            mock.patch.object(protocol, "_run", side_effect=planted_timeout),
            self.assertRaisesRegex(
                protocol.WatchBudgetExhausted,
                "GitHub API call exhausted the watch budget",
            ),
        ):
            protocol.github_runs("rrnewton/hermit", SHA, deadline=120.0)

    def test_repo_policy_routes_workflow_dispatch(self) -> None:
        for index, repo in enumerate(("rrnewton/hermit", "rrnewton/reverie")):
            store = self.root / f"dispatch-{index}.jsonl"
            obligation_id = f"dispatch-{index}"
            obligations.create_obligation(
                repo=repo,
                landed_sha=SHA,
                land_mode="speculative",
                verification_policy=protocol.verification_policy_for_repo(repo),
                actor="test",
                obligation_id=obligation_id,
                path=store,
            )
            now = [0.0]

            def advance(seconds: float) -> None:
                now[0] += seconds

            with (
                self.subTest(repo=repo),
                mock.patch.object(protocol, "github_runs", return_value=[]),
                mock.patch.object(protocol, "github_main_sha", return_value=SHA),
                mock.patch.object(
                    protocol,
                    "_run",
                    return_value=subprocess.CompletedProcess([], 0, "", ""),
                ) as dispatch,
                mock.patch.object(
                    protocol.time, "monotonic", side_effect=lambda: now[0]
                ),
            ):
                protocol.ensure_github_verification(
                    obligation_id,
                    store_path=store,
                    wait_seconds=31,
                    poll_seconds=1,
                    sleep=advance,
                )
            dispatched_files = {call.args[0][4] for call in dispatch.call_args_list}
            self.assertEqual(
                dispatched_files,
                {
                    required["workflow_file"]
                    for required in protocol.verification_policy_for_repo(repo)[
                        "github"
                    ]["required_jobs"]
                },
            )

    def test_github_patch_rejects_wrong_sha_and_wrong_workflow(self) -> None:
        policy = protocol.verification_policy_for_repo("rrnewton/reverie")
        run = hosted_evidence("rrnewton/reverie")[0]
        wrong_sha = {**run, "headSha": NEXT_SHA}
        wrong_workflow = {**run, "workflowName": "Docs"}
        negatives = (
            ([wrong_sha], "expected exact SHA"),
            ([wrong_workflow], "unexpected"),
        )
        self.assertEqual(len(negatives), 2)
        for planted, message in negatives:
            with self.subTest(message=message):
                with self.assertRaisesRegex(protocol.ProtocolError, message):
                    protocol._github_patch(planted, SHA, policy)

    def test_newest_no_result_never_falls_back_to_older_failure(self) -> None:
        policy = protocol.verification_policy_for_repo("rrnewton/hermit")
        runs = protocol._parse_github_runs(
            json.dumps(
                {
                    "total_count": 3,
                    "workflow_runs": [
                        {
                            "id": 10,
                            "head_sha": SHA,
                            "path": protocol.DEFAULT_WORKFLOW_FILE,
                            "name": protocol.DEFAULT_WORKFLOW,
                            "status": "completed",
                            "conclusion": "failure",
                            "created_at": "2026-08-04T15:12:05Z",
                        },
                        {
                            "id": 11,
                            "head_sha": SHA,
                            "path": protocol.DEFAULT_WORKFLOW_FILE,
                            "name": protocol.DEFAULT_WORKFLOW,
                            "status": "completed",
                            "conclusion": "cancelled",
                            "created_at": "2026-08-04T15:24:36Z",
                        },
                        {
                            "id": 12,
                            "head_sha": SHA,
                            "path": protocol.PRIVILEGED_WORKFLOW_FILE,
                            "name": protocol.PRIVILEGED_WORKFLOW,
                            "status": "completed",
                            "conclusion": "success",
                            "created_at": "2026-08-04T15:20:00Z",
                        },
                    ],
                }
            ),
            SHA,
            policy,
        )
        self.assertEqual(runs[0]["databaseId"], 11)

    def test_hosted_authority_requires_registered_nonvacuous_positive_set(self) -> None:
        for repo, expected_count in (
            ("rrnewton/hermit", 1),
            ("rrnewton/reverie", 2),
            (protocol.PARENT_REPO, 4),
        ):
            with self.subTest(repo=repo):
                policy = protocol.verification_policy_for_repo(repo)
                self.assertEqual(
                    policy["github"]["required_positive_count"], expected_count
                )
                self.assertEqual(
                    len(policy["github"]["required_jobs"]), expected_count
                )
                patch = protocol._github_patch(hosted_evidence(repo), SHA, policy)
                self.assertEqual(patch["github"]["state"], "green")
                self.assertEqual(patch["github"]["positive_count"], expected_count)

    def test_parent_hosted_authority_accepts_complete_and_refuses_unbacked(self) -> None:
        """Parent evidence is a counted exact-head set, never a repo bypass."""
        policy = protocol.verification_policy_for_repo(protocol.PARENT_REPO)
        complete = hosted_evidence(protocol.PARENT_REPO)

        accepted = protocol._github_patch(complete, SHA, policy)["github"]
        self.assertEqual(accepted["state"], "green")
        self.assertEqual(accepted["positive_count"], 4)
        self.assertEqual(accepted["required_positive_count"], 4)

        unbacked = json.loads(json.dumps(complete))
        missing_job = policy["github"]["required_jobs"][-1]["job_name"]
        unbacked[-1]["jobs"] = [
            job for job in unbacked[-1]["jobs"] if job["name"] != missing_job
        ]
        refused = protocol._github_patch(unbacked, SHA, policy)["github"]
        self.assertEqual(refused["state"], "no_result")
        self.assertEqual(refused["positive_count"], 3)
        self.assertIn("required job missing", refused["last_poll_error"])

        stale = hosted_evidence(protocol.PARENT_REPO, sha=NEXT_SHA)
        with self.assertRaisesRegex(protocol.ProtocolError, "expected exact SHA"):
            protocol._github_patch(stale, SHA, policy)

    def test_hosted_status_brackets_green_no_result_partial_red_and_stale(self) -> None:
        def observe(repo: str, runs: list[dict]) -> tuple[int, dict]:
            args = argparse.Namespace(repo=repo, sha=SHA, json=True)
            with (
                mock.patch.object(protocol, "github_runs", return_value=runs),
                redirect_stdout(io.StringIO()) as output,
            ):
                rc = protocol.hosted_status(args)
            return rc, json.loads(output.getvalue())

        rc, report = observe("rrnewton/hermit", hosted_evidence("rrnewton/hermit"))
        self.assertEqual(rc, 0)
        self.assertEqual(report["state"], "green")
        self.assertEqual(report["positive_count"], 1)
        self.assertEqual(report["required_positive_count"], 1)

        rc, report = observe("rrnewton/hermit", [])
        self.assertEqual(rc, 4)
        self.assertEqual(report["state"], "no_result")

        partial = hosted_evidence(
            "rrnewton/reverie", job_states=("green", "no_result")
        )
        rc, report = observe("rrnewton/reverie", partial)
        self.assertEqual(rc, 4)
        self.assertEqual(report["positive_count"], 1)
        self.assertEqual(report["required_positive_count"], 2)

        rc, report = observe(
            "rrnewton/hermit",
            hosted_evidence("rrnewton/hermit", job_states=("red",)),
        )
        self.assertEqual(rc, 3)
        self.assertEqual(report["state"], "red")

        stale = hosted_evidence("rrnewton/hermit", sha=NEXT_SHA)
        with self.assertRaisesRegex(protocol.ProtocolError, "expected exact SHA"):
            observe("rrnewton/hermit", stale)

    def test_only_a_dereferenced_producer_can_be_pending_or_running(self) -> None:
        policy = protocol.verification_policy_for_repo("rrnewton/reverie")
        absent = protocol._github_patch([], SHA, policy)["github"]
        self.assertEqual(absent["state"], "no_result")
        self.assertTrue(all(job["state"] == "no_result" for job in absent["jobs"]))

        queued = protocol._github_patch(
            hosted_evidence("rrnewton/reverie", job_states=("pending", "green")),
            SHA,
            policy,
        )["github"]
        self.assertEqual(queued["state"], "pending")
        self.assertIsInstance(queued["jobs"][0]["run_id"], int)
        self.assertIsInstance(queued["jobs"][0]["job_id"], int)
        self.assertTrue(protocol._github_verification_in_flight(queued))

        missing_queued_job = hosted_evidence(
            "rrnewton/reverie", job_states=("pending", "green")
        )
        missing_queued_job[0]["jobs"] = [missing_queued_job[0]["jobs"][1]]
        missing_patch = protocol._github_patch(missing_queued_job, SHA, policy)[
            "github"
        ]
        self.assertEqual(missing_patch["state"], "no_result")
        self.assertEqual(missing_patch["jobs"][0]["state"], "no_result")
        self.assertNotIn("job_id", missing_patch["jobs"][0])
        self.assertFalse(protocol._github_verification_in_flight(missing_patch))

        missing_job_id_evidence = hosted_evidence(
            "rrnewton/reverie", job_states=("pending", "green")
        )
        missing_job_id_evidence[0]["jobs"][0].pop("id")
        missing_job_id_evidence[0]["jobs"] = protocol._parse_github_jobs(
            json.dumps(
                {
                    "total_count": len(missing_job_id_evidence[0]["jobs"]),
                    "jobs": missing_job_id_evidence[0]["jobs"],
                }
            ),
            run=missing_job_id_evidence[0],
            sha=SHA,
        )
        missing_id_patch = protocol._github_patch(
            missing_job_id_evidence, SHA, policy
        )["github"]
        self.assertEqual(missing_id_patch["state"], "no_result")
        self.assertEqual(missing_id_patch["jobs"][0]["state"], "no_result")
        self.assertNotIn("job_id", missing_id_patch["jobs"][0])
        self.assertFalse(protocol._github_verification_in_flight(missing_id_patch))

        mismatched = {
            "local": {"state": "no_result"},
            "github": {
                "state": "pending",
                "run_ids": [11],
                "jobs": [{"state": "pending", "run_id": 12}],
            },
        }
        self.assertFalse(protocol._verification_in_flight(mismatched))
        self.assertTrue(protocol._verification_state_needs_reconcile(mismatched))

        missing_job_id = {
            "local": {"state": "no_result"},
            "github": {
                "state": "pending",
                "run_ids": [11],
                "jobs": [
                    {
                        "state": "pending",
                        "status": "queued",
                        "run_id": 11,
                    }
                ],
            },
        }
        self.assertFalse(protocol._verification_in_flight(missing_job_id))
        self.assertTrue(protocol._verification_state_needs_reconcile(missing_job_id))

    def test_hosted_authority_rejects_missing_skipped_duplicate_and_vacuous_sets(
        self,
    ) -> None:
        policy = protocol.verification_policy_for_repo("rrnewton/reverie")
        missing = hosted_evidence("rrnewton/reverie")
        missing[0]["jobs"].pop()
        self.assertNotEqual(
            protocol._github_patch(missing, SHA, policy)["github"]["state"], "green"
        )
        skipped = hosted_evidence("rrnewton/reverie", job_states=("green", "no_result"))
        self.assertEqual(
            protocol._github_patch(skipped, SHA, policy)["github"]["state"],
            "no_result",
        )
        duplicate = hosted_evidence("rrnewton/reverie")
        duplicate[0]["jobs"].append(dict(duplicate[0]["jobs"][0]))
        with self.assertRaisesRegex(protocol.ProtocolError, "duplicate required"):
            protocol._github_patch(duplicate, SHA, policy)
        reused_identity = hosted_evidence("rrnewton/reverie")
        reused_identity[0]["jobs"][1]["id"] = reused_identity[0]["jobs"][0]["id"]
        with self.assertRaisesRegex(protocol.ProtocolError, "repeats job identity"):
            protocol._github_patch(reused_identity, SHA, policy)
        with self.assertRaisesRegex(protocol.ProtocolError, "repeats job identity"):
            protocol._parse_github_jobs(
                json.dumps(
                    {
                        "total_count": len(reused_identity[0]["jobs"]),
                        "jobs": reused_identity[0]["jobs"],
                    }
                ),
                run=reused_identity[0],
                sha=SHA,
            )
        missing_status = hosted_evidence("rrnewton/reverie")
        for job in missing_status[0]["jobs"]:
            job["status"] = None
        missing_status_patch = protocol._github_patch(missing_status, SHA, policy)
        self.assertEqual(missing_status_patch["github"]["state"], "no_result")
        self.assertEqual(missing_status_patch["github"]["positive_count"], 0)
        missing_updated_at = hosted_evidence("rrnewton/reverie")
        missing_updated_at[0].pop("updatedAt")
        missing_updated_patch = protocol._github_patch(
            missing_updated_at, SHA, policy
        )
        self.assertEqual(missing_updated_patch["github"]["state"], "green")
        self.assertIsNone(missing_updated_patch["github"]["finished_at"])
        hermit_policy = protocol.verification_policy_for_repo("rrnewton/hermit")
        self.assertEqual(hermit_policy["schema_version"], 3)
        self.assertEqual(
            [job["workflow_file"] for job in hermit_policy["github"]["required_jobs"]],
            [protocol.DEFAULT_WORKFLOW_FILE],
        )
        invalid = json.loads(json.dumps(policy))
        invalid["github"]["required_positive_count"] = 0
        with self.assertRaisesRegex(protocol.ProtocolError, "invalid verification"):
            protocol._github_patch([], SHA, invalid)

    def test_unsupported_repo_is_refused_before_arm_writes(self) -> None:
        source = self.root / "source"
        source.mkdir()
        args = argparse.Namespace(
            repo="example/unsupported",
            sha=SHA,
            pr=None,
            source=source,
            store=self.store,
            land_mode="speculative",
            actor="test",
            verification_policy_json=None,
        )
        with mock.patch.object(protocol, "resolve_landed_sha") as resolve:
            with self.assertRaisesRegex(protocol.ProtocolError, "unsupported"):
                protocol.arm(args)
        resolve.assert_not_called()
        self.assertFalse(self.store.exists())

    def test_repo_source_defaults_by_repo_and_rejects_mismatch(self) -> None:
        hermit = self.repo_source("rrnewton/hermit", "mapped-hermit")
        reverie = self.repo_source("rrnewton/reverie", "mapped-reverie")
        parent = self.repo_source(protocol.PARENT_REPO, "mapped-parent")
        agent_utils = self.repo_source(protocol.AGENT_UTILS_REPO, "mapped-agent-utils")
        with mock.patch.dict(
            protocol._DEFAULT_REPO_SOURCES,
            {
                "rrnewton/hermit": hermit,
                "rrnewton/reverie": reverie,
                protocol.PARENT_REPO: parent,
                protocol.AGENT_UTILS_REPO: agent_utils,
            },
            clear=True,
        ):
            self.assertEqual(
                protocol.resolve_repo_source("rrnewton/hermit", None),
                hermit.resolve(),
            )
            self.assertEqual(
                protocol.resolve_repo_source("rrnewton/reverie", None),
                reverie.resolve(),
            )
            self.assertEqual(
                protocol.resolve_repo_source(protocol.PARENT_REPO, None),
                parent.resolve(),
            )
            self.assertEqual(
                protocol.resolve_repo_source(protocol.AGENT_UTILS_REPO, None),
                agent_utils.resolve(),
            )
            with self.assertRaisesRegex(
                protocol.ProtocolError, "not required repository"
            ):
                protocol.resolve_repo_source("rrnewton/reverie", hermit)
            with self.assertRaisesRegex(
                protocol.ProtocolError, "unsupported landing verification repository"
            ):
                protocol.resolve_repo_source("example/unsupported", hermit)

        # Parent ancestry support is paired with a non-vacuous hosted policy;
        # it never falls through to an empty or skipped evidence set.
        parent_policy = protocol.verification_policy_for_repo(protocol.PARENT_REPO)
        self.assertEqual(parent_policy["github"]["required_positive_count"], 4)
        self.assertEqual(len(parent_policy["github"]["required_jobs"]), 4)

    def test_agent_utils_has_landing_ancestry_but_no_post_land_policy(self) -> None:
        """agent-utils is ancestry-verifiable WITHOUT gaining a hosted policy.

        The unpatched default map must carry it (otherwise every agent-utils
        owner directive reports ``unverifiable`` and renders as drift), and the
        post-land policy map must still refuse it (otherwise a repo with zero
        registered jobs would produce a vacuous ``required_positive_count: 0``
        green). Both halves are asserted against the real module state, not a
        patched dict, because the defect being fixed lived in the default.
        """
        self.assertEqual(
            protocol._DEFAULT_REPO_SOURCES[protocol.AGENT_UTILS_REPO],
            protocol.ROOT / "agent-utils",
        )
        self.assertNotIn(
            protocol.AGENT_UTILS_REPO, protocol._CURRENT_VERIFICATION_POLICY_VERSION
        )
        with self.assertRaisesRegex(
            protocol.ProtocolError, "unsupported post-land verification repository"
        ):
            protocol.verification_policy_for_repo(protocol.AGENT_UTILS_REPO)

    def test_repo_source_accepts_intended_https_and_ssh_remote_forms(self) -> None:
        source = self.repo_source("rrnewton/hermit", "remote-forms")
        for remote in (
            "https://github.com/rrnewton/hermit.git",
            "https://github.com:443/rrnewton/hermit/",
            "git@github.com:rrnewton/hermit.git",
            "ssh://git@github.com/rrnewton/hermit.git",
            "ssh://git@github.com:22/rrnewton/hermit/",
        ):
            with self.subTest(remote=remote):
                subprocess.run(
                    ["git", "-C", str(source), "remote", "set-url", "origin", remote],
                    check=True,
                )
                self.assertEqual(
                    protocol.resolve_repo_source("rrnewton/hermit", source),
                    source.resolve(),
                )

    def test_repo_source_rejects_lookalike_hosts_and_non_exact_paths(self) -> None:
        source = self.repo_source("rrnewton/hermit", "lookalike-remotes")
        for remote in (
            "https://evilgithub.com/rrnewton/hermit.git",
            "https://notgithub.com/rrnewton/hermit.git",
            "https://github.com.evil/rrnewton/hermit.git",
            "git@evilgithub.com:rrnewton/hermit.git",
            "https://github.com/rrnewton/reverie.git",
            "https://github.com/rrnewton/hermit/extra.git",
            "https://attacker@github.com/rrnewton/hermit.git",
        ):
            with self.subTest(remote=remote):
                subprocess.run(
                    ["git", "-C", str(source), "remote", "set-url", "origin", remote],
                    check=True,
                )
                with self.assertRaisesRegex(
                    protocol.ProtocolError, "not required repository"
                ):
                    protocol.resolve_repo_source("rrnewton/hermit", source)

    def test_repo_source_keeps_independent_origin_binding_under_wrong_git_dir(
        self,
    ) -> None:
        """Sanitation cannot replace or relax the exact owner/repository check."""

        intended = self.repo_source("rrnewton/hermit", "intended-origin")
        wrong = self.repo_source("rrnewton/reverie", "ambient-wrong-origin")
        env = dict(os.environ)
        env["GIT_DIR"] = str(wrong / ".git")

        unsafe = subprocess.run(
            ["git", "-C", str(intended), "remote", "get-url", "origin"],
            capture_output=True,
            text=True,
            check=True,
            env=env,
        )
        self.assertIn("rrnewton/reverie", unsafe.stdout)
        with mock.patch.dict(os.environ, env, clear=True):
            self.assertEqual(
                intended.resolve(),
                protocol.resolve_repo_source("rrnewton/hermit", intended),
            )
            with self.assertRaisesRegex(
                protocol.ProtocolError, "not required repository"
            ):
                protocol.resolve_repo_source("rrnewton/reverie", intended)

    def test_land_intent_persists_policy_and_refuses_unsupported_repo(self) -> None:
        args = argparse.Namespace(
            repo="rrnewton/reverie",
            pr=378,
            source=self.repo_source("rrnewton/reverie", "reverie-intent"),
            land_mode="speculative",
            actor="test",
            store=self.store,
            github_wait_seconds=5,
            poll_seconds=1,
        )
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        self.assertEqual(
            intent["verification_policy"],
            protocol.verification_policy_for_repo("rrnewton/reverie"),
        )
        args.repo = "example/unsupported"
        with self.assertRaisesRegex(protocol.ProtocolError, "unsupported"):
            land_and_arm._new_intent(args, ["/bin/true"])

    def test_land_intent_policy_is_forwarded_to_initial_obligation_arm(self) -> None:
        args = argparse.Namespace(
            repo="rrnewton/reverie",
            pr=378,
            source=self.repo_source("rrnewton/reverie", "reverie-forward"),
            land_mode="speculative",
            actor="test",
            store=self.store,
            github_wait_seconds=5,
            poll_seconds=1,
        )
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        with mock.patch.object(protocol, "main", return_value=2) as arm:
            code, obligation_id = land_and_arm.arm_sha(intent, SHA)
        self.assertEqual(code, 2)
        self.assertIsNone(obligation_id)
        arguments = arm.call_args.args[0]
        raw_policy = arguments[arguments.index("--verification-policy-json") + 1]
        self.assertEqual(json.loads(raw_policy), intent["verification_policy"])

    def test_existing_legacy_obligation_is_bound_before_arm_returns(self) -> None:
        args = argparse.Namespace(
            repo="rrnewton/reverie",
            pr=378,
            source=self.repo_source("rrnewton/reverie", "reverie-existing"),
            land_mode="speculative",
            actor="test",
            store=self.store,
            github_wait_seconds=5,
            poll_seconds=1,
        )
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        obligations.create_obligation(
            repo=args.repo,
            landed_sha=SHA,
            land_mode=args.land_mode,
            actor=args.actor,
            obligation_id="legacy-existing",
            path=self.store,
        )

        def resume(_arguments):
            protocol.bind_verification_policy(
                "legacy-existing",
                self.store,
                requested_policy=intent["verification_policy"],
            )
            obligations.transition(
                "legacy-existing", "launch-armed", durable_launch_patch(), self.store
            )
            return 0

        with mock.patch.object(protocol, "main", side_effect=resume) as arm:
            code, obligation_id = land_and_arm.arm_sha(intent, SHA)
        arm.assert_called_once()
        self.assertEqual(code, 0)
        self.assertEqual(obligation_id, "legacy-existing")
        record = obligations.get_record("legacy-existing", self.store)
        self.assertEqual(record["verification_policy"], intent["verification_policy"])

    def test_duplicate_arm_selects_current_open_record_not_older_closed_record(
        self,
    ) -> None:
        source = self.repo_source("rrnewton/reverie", "reverie-duplicate")
        args = argparse.Namespace(
            repo="rrnewton/reverie",
            pr=378,
            source=source,
            land_mode="speculative",
            actor="test",
            store=self.store,
            github_wait_seconds=5,
            poll_seconds=1,
        )
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        obligations.create_obligation(
            repo=args.repo,
            landed_sha=SHA,
            land_mode=args.land_mode,
            verification_policy=intent["verification_policy"],
            obligation_id="older-closed",
            path=self.store,
        )
        obligations.transition(
            "older-closed",
            "satisfied",
            {"overall_state": "satisfied", **durable_launch_patch()},
            self.store,
        )
        obligations.create_obligation(
            repo=args.repo,
            landed_sha=SHA,
            land_mode=args.land_mode,
            obligation_id="current-open",
            path=self.store,
        )

        def resume(_arguments):
            protocol.bind_verification_policy(
                "current-open",
                self.store,
                requested_policy=intent["verification_policy"],
            )
            obligations.transition(
                "current-open", "launch-armed", durable_launch_patch(), self.store
            )
            return 0

        with mock.patch.object(protocol, "main", side_effect=resume) as arm:
            code, obligation_id = land_and_arm.arm_sha(intent, SHA)
        arm.assert_called_once()
        self.assertEqual((code, obligation_id), (0, "current-open"))
        self.assertEqual(
            obligations.get_record("current-open", self.store)["verification_policy"],
            intent["verification_policy"],
        )

    def test_new_arm_returns_newly_satisfied_record_not_older_closed_record(
        self,
    ) -> None:
        source = self.repo_source("rrnewton/reverie", "reverie-newly-satisfied")
        args = argparse.Namespace(
            repo="rrnewton/reverie",
            pr=378,
            source=source,
            land_mode="speculative",
            actor="test",
            store=self.store,
            github_wait_seconds=5,
            poll_seconds=1,
        )
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        obligations.create_obligation(
            repo=args.repo,
            landed_sha=SHA,
            land_mode=args.land_mode,
            verification_policy=intent["verification_policy"],
            obligation_id="older-closed",
            path=self.store,
        )
        obligations.transition(
            "older-closed",
            "satisfied",
            {"overall_state": "satisfied", **durable_launch_patch()},
            self.store,
        )

        def arm(_arguments):
            obligations.create_obligation(
                repo=args.repo,
                landed_sha=SHA,
                land_mode=args.land_mode,
                verification_policy=intent["verification_policy"],
                obligation_id="newly-satisfied",
                path=self.store,
            )
            obligations.transition(
                "newly-satisfied",
                "satisfied",
                {"overall_state": "satisfied", **durable_launch_patch()},
                self.store,
            )
            return 0

        with mock.patch.object(protocol, "main", side_effect=arm):
            code, obligation_id = land_and_arm.arm_sha(intent, SHA)
        self.assertEqual((code, obligation_id), (0, "newly-satisfied"))

    def test_arm_persists_policy_in_initial_opened_event(self) -> None:
        source = self.repo_source("rrnewton/reverie", "reverie-arm")
        args = argparse.Namespace(
            repo="rrnewton/reverie",
            sha=REVERIE_LANDED_SHA,
            pr=None,
            source=source,
            store=self.store,
            land_mode="speculative",
            actor="test",
            verification_policy_json=None,
            github_wait_seconds=1,
            poll_seconds=1,
            no_dispatch=False,
        )
        estimate = {
            "kind": "unknown",
            "wall_seconds": 0.0,
            "cpu_seconds": 0.0,
            "basis": "test",
        }

        def register_spawn(arguments, _log_path):
            arguments = list(arguments)
            obligation_id = (
                arguments[1] if arguments[0] == "_local-run" else arguments[2]
            )
            token = arguments[arguments.index("--launch-token") + 1]
            store = Path(arguments[arguments.index("--store") + 1])
            if arguments[0] == "_local-run":
                protocol._register_local_runner(obligation_id, token, store, pid=101)
                return 101
            protocol._register_watcher(obligation_id, token, store, pid=102)
            return 102

        with (
            mock.patch.object(
                protocol, "resolve_landed_sha", return_value=REVERIE_LANDED_SHA
            ),
            mock.patch.object(
                protocol, "estimate_local_validate_cost", return_value=estimate
            ) as estimate_cost,
            mock.patch.object(
                protocol, "_spawn_detached", side_effect=register_spawn
            ) as spawn,
            mock.patch.object(
                protocol,
                "_watcher_process_identity",
                return_value={
                    "host": "test-host",
                    "boot_id": "test-boot",
                    "start_ticks": 1020,
                },
            ),
            mock.patch.object(protocol, "_pid_alive", return_value=True),
            mock.patch.object(protocol, "ensure_github_verification"),
            redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(protocol.arm(args), 0)
        opened = json.loads(self.store.read_text().splitlines()[0])
        self.assertEqual(opened["event_type"], "opened")
        self.assertEqual(
            opened["verification_policy"],
            protocol.verification_policy_for_repo("rrnewton/reverie"),
        )
        armed = obligations.get_record(opened["obligation_id"], self.store)
        estimate_cost.assert_not_called()
        spawn.assert_called_once()
        self.assertEqual(spawn.call_args.args[0][0], "watch")
        # `obligation_launch_durable` asks `_pid_alive` whether the recorded
        # watcher pid is running. The fixture's pids (101/102) are arbitrary, so
        # keep `_pid_alive` stubbed while asserting: outside the stub this
        # assertion silently becomes "does pid 102 happen to exist on this host",
        # which is true on a many-core box (kernel threads reach into the low
        # hundreds) and false on a small CI runner.
        with mock.patch.object(protocol, "_pid_alive", return_value=True):
            self.assertTrue(protocol.obligation_launch_durable(armed))
        self.assertTrue(protocol._local_policy_skip_valid(armed))
        self.assertEqual(armed["watcher"]["state"], "running")
        self.assertEqual(armed["watcher"]["identity"]["start_ticks"], 1020)

    def test_recovery_reuses_live_legacy_producers_without_duplicate_spawn(
        self,
    ) -> None:
        self.create()
        obligations.transition(
            "test-obligation",
            "legacy-live-producers",
            {
                "local": {
                    "state": "running",
                    "pid": protocol.os.getpid(),
                    "started_at": "2026-08-05T00:10:00Z",
                    "launch_token": None,
                },
                "watcher": {
                    "state": None,
                    "pid": protocol.os.getpid(),
                    "started_at": "2026-08-05T00:10:00Z",
                    "launch_token": None,
                },
            },
            self.store,
        )
        with (
            mock.patch.object(protocol, "_spawn_detached") as spawn,
            mock.patch.object(protocol, "ensure_github_verification"),
        ):
            record, error = protocol.resume_obligation_launch(
                "test-obligation",
                source=self.root,
                store_path=self.store,
                github_wait_seconds=1,
                poll_seconds=1,
                allow_dispatch=False,
            )
        spawn.assert_not_called()
        self.assertIsNone(error)
        self.assertTrue(protocol.obligation_launch_durable(record))

    def test_live_launch_owner_blocks_duplicate_resumer(self) -> None:
        self.create()
        obligations.transition(
            "test-obligation",
            "launch-claimed",
            {
                "launch": {
                    "state": "launching",
                    "token": "first-owner",
                    "launcher_pid": 98765,
                    "attempt": 1,
                }
            },
            self.store,
        )
        with mock.patch.object(protocol, "_pid_alive", return_value=True):
            with self.assertRaisesRegex(protocol.LaunchBusy, "live pid 98765"):
                protocol._claim_obligation_launch("test-obligation", self.store)
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["launch"]["token"], "first-owner")
        self.assertEqual(record["launch"]["attempt"], 1)

    def test_concurrent_launch_claims_choose_one_owner_without_duplicates(self) -> None:
        self.create()
        ready = threading.Barrier(2)

        def claim() -> str:
            ready.wait()
            try:
                return protocol._claim_obligation_launch("test-obligation", self.store)[
                    0
                ]
            except protocol.LaunchBusy:
                return "busy"

        with ThreadPoolExecutor(max_workers=2) as executor:
            futures = [executor.submit(claim) for _ in range(2)]
            outcomes = [future.result() for future in futures]
        self.assertCountEqual(outcomes, ["owner", "busy"])
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["launch"]["state"], "launching")
        self.assertEqual(record["launch"]["attempt"], 1)

    def test_green_launch_requires_full_persisted_receipt_binding(self) -> None:
        self.create()
        obligations.transition(
            "test-obligation",
            "forged-receipt-state",
            {
                "launch": {"state": "armed"},
                "local": {
                    "state": "green",
                    "launch_token": "registered-token",
                    "registered_at": "2026-08-05T00:09:00Z",
                    "started_at": "2026-08-05T00:09:01Z",
                    "pid": protocol.os.getpid(),
                    "finished_at": "2026-08-05T00:11:00Z",
                    "receipt_verification": {"state": "verified"},
                },
                "watcher": {
                    "state": "running",
                    "pid": protocol.os.getpid(),
                    "started_at": "2026-08-05T00:10:00Z",
                },
            },
            self.store,
        )
        record = obligations.get_record("test-obligation", self.store)
        self.assertFalse(protocol._local_launch_durable(record))
        self.assertFalse(protocol.obligation_launch_durable(record))

    def test_persisted_receipt_matches_fresh_canonical_selection(self) -> None:
        evidence = verified_local_receipt()
        self.assertTrue(
            protocol._persisted_local_receipt_valid(
                evidence, repo="rrnewton/hermit", sha=SHA
            )
        )
        self.current_local_receipt.assert_called_once_with("rrnewton/hermit", SHA)
        self.current_local_receipt.reset_mock()
        record = {
            "repo": "rrnewton/hermit",
            "landed_sha": SHA,
            "local": local_green(),
        }
        self.assertTrue(protocol._local_launch_durable(record))
        self.current_local_receipt.assert_called_once_with("rrnewton/hermit", SHA)

    def test_unrelated_newer_valid_receipt_does_not_invalidate_persisted_green(self) -> None:
        persisted = verified_local_receipt()
        current = verified_local_receipt()
        old = current["report"]["newest_qualifying"]
        newer = json.loads(json.dumps(old))
        newer["finished_at"] = "2026-08-05T00:12:00Z"
        newer["receipt_identity"]["digest"] = "e" * 64
        newer["receipt_identity"]["tuple"]["finished_at"] = newer["finished_at"]
        current["report"]["newest_qualifying"] = newer
        current["report"]["qualifying_count"] = 2
        current["report"]["qualifying_receipts"] = [old, newer]
        canonical = json.dumps(
            current["report"], sort_keys=True, separators=(",", ":")
        ).encode()
        current["report_sha256"] = hashlib.sha256(canonical).hexdigest()
        self.current_local_receipt.return_value = (True, current)

        self.assertTrue(
            protocol._persisted_local_receipt_valid(
                persisted, repo="rrnewton/hermit", sha=SHA
            )
        )

    def test_recomputed_outer_hash_cannot_authorize_tampered_receipt(self) -> None:
        tampered = verified_local_receipt()
        selected = tampered["report"]["newest_qualifying"]
        selected["receipt_identity"]["digest"] = "0" * 64
        canonical = json.dumps(
            tampered["report"], sort_keys=True, separators=(",", ":")
        ).encode()
        # This is the old exploit: the unkeyed outer hash can be recomputed
        # after changing the alleged selected receipt.
        tampered["report_sha256"] = hashlib.sha256(canonical).hexdigest()

        self.assertFalse(
            protocol._persisted_local_receipt_valid(
                tampered, repo="rrnewton/hermit", sha=SHA
            )
        )
        tampered_fields = verified_local_receipt()
        selected_fields = tampered_fields["report"]["newest_qualifying"]
        selected_fields["host"] = "attacker-host"
        selected_fields["receipt_identity"]["tuple"]["host"] = "attacker-host"
        canonical_fields = json.dumps(
            tampered_fields["report"], sort_keys=True, separators=(",", ":")
        ).encode()
        tampered_fields["report_sha256"] = hashlib.sha256(
            canonical_fields
        ).hexdigest()
        self.assertFalse(
            protocol._persisted_local_receipt_valid(
                tampered_fields, repo="rrnewton/hermit", sha=SHA
            )
        )
        record = {
            "repo": "rrnewton/hermit",
            "landed_sha": SHA,
            "local": {**local_green(), "receipt_verification": tampered},
        }
        self.assertFalse(protocol._local_launch_durable(record))

        self.create()
        obligations.transition(
            "test-obligation",
            "tampered-persisted-green",
            {
                "local": record["local"],
                "github": {"state": "no_result"},
            },
            self.store,
        )
        rebound = protocol.bind_local_receipt_authority("test-obligation", self.store)
        self.assertEqual(rebound["local"]["state"], "no_result")
        self.assertIn(
            "canonical verifier did not carry its complete qualifying receipt set",
            rebound["local"]["classification_reason"],
        )

    def test_executed_terminal_requires_registered_producer_or_policy_skip(
        self,
    ) -> None:
        record = {
            "repo": "rrnewton/hermit",
            "landed_sha": SHA,
            "local": {
                "state": "no_result",
                "finished_at": "2026-08-05T00:11:00Z",
            },
        }
        self.assertFalse(protocol._local_launch_durable(record))

        record["local"]["launch_token"] = "token-only"
        self.assertFalse(protocol._local_launch_durable(record))

        record["local"].update(
            registered_at="2026-08-05T00:09:00Z",
            started_at="2026-08-05T00:09:01Z",
        )
        self.assertFalse(protocol._local_launch_durable(record))
        record["local"]["pid"] = protocol.os.getpid()
        self.assertTrue(protocol._local_launch_durable(record))

        for repo, sha in (
            ("rrnewton/reverie", REVERIE_LANDED_SHA),
            (protocol.PARENT_REPO, SHA),
        ):
            with self.subTest(repo=repo):
                policy_record = {
                    "repo": repo,
                    "landed_sha": sha,
                    "local": protocol._local_policy_skip_patch(repo),
                }
                self.assertTrue(protocol._local_launch_durable(policy_record))
                policy_record["local"]["policy_skip"]["repo"] = "rrnewton/hermit"
                self.assertFalse(protocol._local_launch_durable(policy_record))

    def test_parent_local_not_applicable_cannot_authorize_without_hosted_green(
        self,
    ) -> None:
        obligation_id = "parent-hosted-only-authority"
        policy = protocol.verification_policy_for_repo(protocol.PARENT_REPO)
        obligations.create_obligation(
            repo=protocol.PARENT_REPO,
            landed_sha=SHA,
            land_mode="speculative",
            verification_policy=policy,
            actor="test",
            obligation_id=obligation_id,
            path=self.store,
        )

        record = protocol.bind_local_receipt_authority(obligation_id, self.store)
        self.assertEqual(record["local"]["state"], "no_result")
        self.assertEqual(record["local"]["policy_skip"]["outcome"], "not_applicable")
        record = protocol.evaluate_obligation(obligation_id, store_path=self.store)
        self.assertEqual(record["overall_state"], "open")

        obligations.transition(
            obligation_id,
            "parent-hosted-observed",
            protocol._github_patch(
                hosted_evidence(protocol.PARENT_REPO), SHA, policy
            ),
            self.store,
        )
        record = protocol.evaluate_obligation(obligation_id, store_path=self.store)
        self.assertEqual(record["overall_state"], "satisfied")
        self.assertEqual(record["github"]["positive_count"], 4)

    def test_legacy_reverie_registered_terminal_is_rebound_to_policy_skip(
        self,
    ) -> None:
        obligation_id = "legacy-reverie-terminal"
        obligations.create_obligation(
            repo="rrnewton/reverie",
            landed_sha=REVERIE_LANDED_SHA,
            land_mode="speculative",
            verification_policy=protocol.verification_policy_for_repo(
                "rrnewton/reverie"
            ),
            actor="test",
            obligation_id=obligation_id,
            path=self.store,
        )
        obligations.transition(
            obligation_id,
            "legacy-registered-red",
            {
                "local": {
                    "state": "red",
                    "launch_token": "legacy-token",
                    "registered_at": "2026-08-05T00:09:00Z",
                    "started_at": "2026-08-05T00:09:01Z",
                    "finished_at": "2026-08-05T00:11:00Z",
                    "pid": protocol.os.getpid(),
                }
            },
            self.store,
        )
        with (
            mock.patch.object(protocol, "_spawn_detached") as spawn,
            mock.patch.object(protocol, "estimate_local_validate_cost") as estimate,
        ):
            record = protocol._ensure_local_launched(
                obligation_id, self.root, self.store
            )
        spawn.assert_not_called()
        estimate.assert_not_called()
        self.assertTrue(protocol._local_policy_skip_valid(record))
        record = protocol.evaluate_obligation(obligation_id, store_path=self.store)
        self.assertEqual(record["overall_state"], "open")
        self.assertEqual(record["local"]["state"], "no_result")

    def test_legacy_reverie_obligation_recovers_append_only_via_rust_workflow(
        self,
    ) -> None:
        obligation_id = "20260805-054331-025d37800d34-963f83"
        obligations.create_obligation(
            repo="rrnewton/reverie",
            landed_sha=REVERIE_LANDED_SHA,
            land_mode="speculative",
            actor="test",
            obligation_id=obligation_id,
            path=self.store,
        )
        obligations.transition(
            obligation_id,
            "incident-shape",
            {
                "local": {"state": "green", "exit_code": 0},
                "github": {
                    "state": "no_result",
                    "last_poll_error": "HTTP 404: workflow ci-portable.yml not found",
                },
            },
            self.store,
        )
        before = self.store.read_bytes()
        run_payload = {
            "total_count": 1,
            "workflow_runs": [
                {
                    "id": 30978954323,
                    "head_sha": REVERIE_LANDED_SHA,
                    "path": protocol.REVERIE_WORKFLOW_FILE,
                    "name": protocol.REVERIE_WORKFLOW,
                    "status": "completed",
                    "conclusion": "success",
                    "created_at": "2026-08-05T05:42:50Z",
                    "run_started_at": "2026-08-05T05:42:50Z",
                    "updated_at": "2026-08-05T05:52:36Z",
                    "html_url": "https://github.com/rrnewton/reverie/actions/runs/30978954323",
                    "event": "push",
                }
            ],
        }
        jobs_payload = {
            "total_count": 2,
            "jobs": [
                {
                    "id": 92219055209 + index,
                    "run_id": 30978954323,
                    "head_sha": REVERIE_LANDED_SHA,
                    "name": required["job_name"],
                    "status": "completed",
                    "conclusion": "success",
                    "started_at": "2026-08-05T05:43:00Z",
                    "completed_at": "2026-08-05T05:52:00Z",
                    "html_url": f"https://github.invalid/job/{index}",
                }
                for index, required in enumerate(
                    protocol.verification_policy_for_repo("rrnewton/reverie")["github"][
                        "required_jobs"
                    ]
                )
            ],
        }
        responses = [
            subprocess.CompletedProcess([], 0, json.dumps(run_payload), ""),
            subprocess.CompletedProcess([], 0, json.dumps(jobs_payload), ""),
        ]
        with mock.patch.object(protocol, "_run", side_effect=responses) as run:
            record = protocol.poll_obligation(obligation_id, self.store)
        self.assertEqual(record["overall_state"], "satisfied")
        self.assertEqual(record["local"]["state"], "no_result")
        self.assertIsNone(record["local"]["receipt_verification"])
        self.assertEqual(
            record["local"]["policy_skip"]["authority"],
            protocol.LOCAL_POLICY_SKIP_AUTHORITY,
        )
        self.assertEqual(record["github"]["run_ids"], [30978954323])
        self.assertTrue(self.store.read_bytes().startswith(before))
        events = [json.loads(line) for line in self.store.read_text().splitlines()]
        self.assertIn(
            "verification-policy-bound", [event["event_type"] for event in events]
        )
        self.assertIn(
            "repos/rrnewton/reverie/actions/runs?head_sha=",
            run.call_args_list[0].args[0][-1],
        )
        self.assertIn("/30978954323/jobs?", run.call_args_list[1].args[0][-1])

    def test_reverie_bare_local_green_is_not_preserved_without_hosted_result(
        self,
    ) -> None:
        obligation_id = "reverie-poll-error"
        obligations.create_obligation(
            repo="rrnewton/reverie",
            landed_sha=REVERIE_LANDED_SHA,
            land_mode="speculative",
            actor="test",
            obligation_id=obligation_id,
            path=self.store,
        )
        obligations.transition(
            obligation_id,
            "incident-shape",
            {
                "local": {"state": "green", "exit_code": 0},
                "github": {"state": "no_result"},
            },
            self.store,
        )
        with (
            mock.patch.object(
                protocol,
                "github_runs",
                side_effect=protocol.ProtocolError("simulated GitHub query failure"),
            ) as runs,
            mock.patch.object(protocol, "verify_local_receipt") as verify_receipt,
        ):
            first = protocol.poll_obligation(obligation_id, self.store)
            second = protocol.poll_obligation(obligation_id, self.store)
        verify_receipt.assert_not_called()
        self.assertEqual(first["overall_state"], "open")
        self.assertEqual(second["overall_state"], "open")
        self.assertEqual(first["local"]["state"], "no_result")
        self.assertEqual(
            first["local"]["policy_skip"]["authority"],
            protocol.LOCAL_POLICY_SKIP_AUTHORITY,
        )
        self.assertEqual(runs.call_count, 2)
        events = [json.loads(line) for line in self.store.read_text().splitlines()]
        self.assertEqual(
            sum(event["event_type"] == "github-poll-error" for event in events), 2
        )

    def test_invalid_persisted_policy_cannot_be_or_satisfied(self) -> None:
        obligation_id = "invalid-policy"
        invalid_policy = {
            **protocol.verification_policy_for_repo("rrnewton/reverie"),
            "github": {"workflow_file": "docs.yml", "workflow_name": "Docs"},
        }
        obligations.create_obligation(
            repo="rrnewton/reverie",
            landed_sha=REVERIE_LANDED_SHA,
            land_mode="speculative",
            verification_policy=invalid_policy,
            actor="test",
            obligation_id=obligation_id,
            path=self.store,
        )
        obligations.transition(
            obligation_id,
            "local-green",
            {
                "local": {"state": "green", "exit_code": 0},
                "github": {"state": "no_result"},
            },
            self.store,
        )
        with (
            mock.patch.object(protocol, "github_runs") as runs,
            redirect_stderr(io.StringIO()),
        ):
            direct = protocol.evaluate_obligation(
                obligation_id, store_path=self.store, main_sha=REVERIE_LANDED_SHA
            )
            record = protocol.poll_obligation(obligation_id, self.store)
        runs.assert_not_called()
        self.assertEqual(direct["overall_state"], "investigation_required")
        self.assertEqual(record["overall_state"], "investigation_required")
        self.assertEqual(record["failure_source"], "verification_policy")
        self.assertEqual(record["remediation"]["state"], "not_required")
        self.assertTrue(protocol._watch_complete(record))

    def test_estimate_uses_only_recorded_history(self) -> None:
        ledger = self.root / "ledger.jsonl"
        ledger.write_text(
            "\n".join(
                json.dumps(
                    {
                        "profile": "full",
                        "real_seconds": wall,
                        "user_seconds": cpu,
                        "sys_seconds": 1,
                    }
                )
                for wall, cpu in ((100, 200), (5000, 9000), (2000, 8000))
            )
            + "\n"
        )
        estimate = protocol.estimate_local_validate_cost(ledger)
        self.assertEqual(estimate["kind"], "derived")
        self.assertEqual(estimate["wall_seconds"], 5000)
        self.assertEqual(estimate["cpu_seconds"], 9001)
        self.assertIn("last 3 usable", estimate["basis"])

    def test_estimate_is_unknown_without_usable_history(self) -> None:
        estimate = protocol.estimate_local_validate_cost(self.root / "missing.jsonl")
        self.assertEqual(estimate["kind"], "unknown")
        self.assertIsNone(estimate["wall_seconds"])
        self.assertIsNone(estimate["cpu_seconds"])
        self.assertTrue(estimate["basis"].startswith("not measured:"))

    def test_lone_local_red_is_provisional_not_an_immediate_revert(self) -> None:
        # Regression (task obligation-path-must-consume-no-result-taxonomy): a
        # single local red whose GitHub leg has not corroborated it once drove an
        # automated revert of a healthy main tip (e8a0d8d3); re-validation passed.
        # An uncorroborated, un-reproduced local red must NOT remediate.
        self.create()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "finished_at": "2026-08-03T01:00:00Z",
                    "exit_code": 1,
                    "log_path": "/tmp/local.log",
                    "redispatch_count": 0,
                },
                "github": {"state": "running"},
            }
        )
        with redirect_stderr(io.StringIO()):
            record = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=SHA
            )
        self.assertNotEqual(record["overall_state"], "remediation_required")

    def test_ledger_red_at_tip_awaits_owner_without_dispatch(
        self,
    ) -> None:
        # C: even the authoritative ledger red cannot mutate main by itself.
        self.create()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "finished_at": "2026-08-03T01:00:00Z",
                    "exit_code": 1,
                    "log_path": "/tmp/local.log",
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                },
                "github": {"state": "no_result"},
            }
        )
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            record = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=SHA
            )
        self.assertEqual(record["overall_state"], "remediation_required")
        self.assertEqual(record["recommendation"]["action"], "revert")
        self.assertEqual(record["remediation"]["state"], "awaiting_owner")
        self.assertIsNone(record["remediation"].get("dispatch"))
        self.assertIn("AWAITING OWNER", stderr.getvalue())
        self.assertNotIn("REMEDIATION TRIGGERED", stderr.getvalue())

    def test_owner_revert_authorization_is_consumed_by_cas(self) -> None:
        self.create()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "finished_at": "2026-08-10T01:00:00Z",
                    "exit_code": 1,
                    "log_path": "/tmp/local.log",
                },
                "github": {"state": "no_result"},
            }
        )
        with redirect_stderr(io.StringIO()):
            awaiting = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=SHA
            )
            accepted = protocol.authorize_revert(
                awaiting,
                self.revert_snapshot(),
                store_path=self.store,
                now=datetime(2026, 8, 10, tzinfo=timezone.utc),
            )
        self.assertEqual(accepted["remediation"]["state"], "triggered")
        authorization = accepted["remediation"]["authorization"]
        self.assertEqual(authorization["state"], "consumed")
        self.assertEqual(authorization["main_sha"], SHA)
        self.assertEqual(authorization["candidate_sha"], NEXT_SHA)
        self.assertEqual(
            accepted["remediation"]["dispatch"]["state"], "pending"
        )
        with self.assertRaisesRegex(protocol.ProtocolError, "already consumed"):
            protocol.authorize_revert(
                awaiting,
                self.revert_snapshot(),
                store_path=self.store,
                now=datetime(2026, 8, 10, tzinfo=timezone.utc),
            )

    def test_revert_authorization_refuses_all_five_mutations(self) -> None:
        self.create()
        record = obligations.get_record("test-obligation", self.store)
        now = datetime(2026, 8, 10, tzinfo=timezone.utc)
        cases = {
            "wrong-candidate": self.revert_snapshot(candidate_sha="d" * 40),
            "wrong-main": self.revert_snapshot(main_sha="e" * 40),
            "non-owner": self.revert_snapshot(
                issuer="not-owner", association="MEMBER"
            ),
            "expired": self.revert_snapshot(expires_at="2026-08-09T00:00:00Z"),
            "reused-nonce": self.revert_snapshot(),
        }
        for name, snapshot in cases.items():
            used = (
                {"test-revert-nonce-0001"} if name == "reused-nonce" else set()
            )
            accepted, refusals = protocol.assess_revert_authorization(
                record, snapshot, used_nonces=used, now=now
            )
            self.assertFalse(accepted, name)
            self.assertGreaterEqual(len(refusals), 1, name)
        self.assertEqual(len(cases), 5)

    def test_main_identity_unavailable_warns_without_action(self) -> None:
        self.create()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "finished_at": "2026-08-10T01:00:00Z",
                    "exit_code": 1,
                    "log_path": "/tmp/local.log",
                },
                "github": {"state": "no_result"},
            }
        )
        stderr = io.StringIO()
        with (
            mock.patch.object(
                protocol,
                "github_main_sha",
                side_effect=protocol.ProtocolError("API unavailable"),
            ),
            redirect_stderr(stderr),
        ):
            record = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store
            )
        self.assertEqual(record["overall_state"], "remediation_required")
        self.assertEqual(
            record["remediation"]["state"], "awaiting_main_identity"
        )
        self.assertIsNone(record["recommendation"])
        self.assertIsNone(record["remediation"].get("dispatch"))
        self.assertIn("NO ACTION DISPATCHED", stderr.getvalue())

    def test_spent_local_red_waits_while_github_still_running(self) -> None:
        # SEQUENCING regression (task cancellation_taxonomy_distinguish_self,
        # obligation 20260804-025419-0f891e43): a local red that had spent its whole
        # re-dispatch budget armed action=revert while the GitHub verify run was
        # STILL RUNNING -- the tool's own exoneration path in flight, acted on
        # before it was read. A running leg must be waited for: NOTHING arms until
        # both legs report.
        self.create()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "finished_at": "2026-08-04T03:04:59Z",
                    "exit_code": 1,
                    "log_path": "/tmp/local.log",
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                },
                "github": {"state": "running"},
            }
        )
        with redirect_stderr(io.StringIO()):
            record = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=SHA
            )
        self.assertNotEqual(record["overall_state"], "remediation_required")
        self.assertIsNone(record.get("recommendation"))

    def test_github_red_waits_while_local_leg_is_active(self) -> None:
        # Count both active pre-answer states. A GitHub red must not actuate while
        # the local verifier is starting or running and can still exonerate it.
        active_states = ("starting", "running")
        self.assertEqual(len(active_states), 2)
        for index, local_state in enumerate(active_states):
            store = self.root / f"active-local-{index}.jsonl"
            obligation_id = f"active-local-{index}"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="speculative",
                verification_policy=protocol.verification_policy_for_repo(
                    "rrnewton/hermit"
                ),
                actor="test",
                obligation_id=obligation_id,
                path=store,
            )
            obligations.transition(
                obligation_id,
                "legs",
                {
                    "local": {"state": local_state},
                    "github": {
                        "state": "red",
                        "finished_at": "2026-08-04T03:00:00Z",
                    },
                },
                store,
            )
            with (
                mock.patch.object(protocol, "trigger_remediation") as actuator,
                redirect_stderr(io.StringIO()),
            ):
                record = protocol.evaluate_obligation(
                    obligation_id, store_path=store, main_sha=SHA
                )
            actuator.assert_not_called()
            self.assertNotEqual(record["overall_state"], "remediation_required")

    def test_hermit_ledger_red_precedes_hosted_green_but_never_auto_reverts(
        self,
    ) -> None:
        # Hermit's two orientations are intentionally asymmetric: ledger red is
        # authoritative but owner-gated; ledger green satisfies despite hosted red.
        cases = (("red", "green", "local"), ("green", "red", "github"))
        self.assertEqual(len(cases), 2)
        for index, (local_state, github_state, failed_source) in enumerate(cases):
            store = self.root / f"disagreement-{index}.jsonl"
            obligation_id = f"disagreement-{index}"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="speculative",
                verification_policy=protocol.verification_policy_for_repo(
                    "rrnewton/hermit"
                ),
                actor="test",
                obligation_id=obligation_id,
                path=store,
            )
            obligations.transition(
                obligation_id,
                "legs",
                {
                    "local": (
                        local_green()
                        if local_state == "green"
                        else {"state": local_state}
                    ),
                    "github": {"state": github_state},
                },
                store,
            )
            stderr = io.StringIO()
            with (
                mock.patch.object(protocol, "trigger_remediation") as actuator,
                redirect_stderr(stderr),
            ):
                record = protocol.evaluate_obligation(
                    obligation_id, store_path=store, main_sha=SHA
                )
            actuator.assert_not_called()
            if local_state == "red":
                self.assertEqual(record["overall_state"], "remediation_required")
                self.assertEqual(record["failure_source"], failed_source)
                self.assertEqual(record["remediation"]["state"], "awaiting_owner")
                self.assertIn("AWAITING OWNER", stderr.getvalue())
            else:
                self.assertEqual(record["overall_state"], "satisfied")
                self.assertEqual(record["remediation"]["state"], "not_required")

    def test_poll_observes_late_red_after_nonblocking_satisfaction(self) -> None:
        self.create()
        self.transition({"local": {"state": "green"}})
        running_output = hosted_evidence("rrnewton/hermit", job_states=("running",))
        with mock.patch.object(protocol, "github_runs", return_value=running_output):
            satisfied = protocol.poll_obligation("test-obligation", self.store)
        self.assertEqual(satisfied["overall_state"], "satisfied")
        self.assertEqual(satisfied["github"]["state"], "running")
        self.assertFalse(protocol._watch_complete(satisfied))
        late_red = hosted_evidence("rrnewton/hermit", job_states=("red",))
        with (
            mock.patch.object(protocol, "github_runs", return_value=late_red) as runs,
            mock.patch.object(protocol, "trigger_remediation") as actuator,
            redirect_stdout(io.StringIO()),
            redirect_stderr(io.StringIO()),
        ):
            # No --id: this is the restart/global-recovery consumer. It must select
            # the already-satisfied record because its hosted producer is in flight.
            protocol.watch(
                store_path=self.store,
                obligation_id=None,
                once=True,
                poll_seconds=1,
            )
        reopened = obligations.get_record("test-obligation", self.store)
        runs.assert_called_once()
        actuator.assert_not_called()
        self.assertEqual(reopened["overall_state"], "satisfied")
        self.assertEqual(reopened["remediation"]["state"], "not_required")
        self.assertTrue(protocol._watch_complete(reopened))

    def test_global_watch_clears_fake_pending_when_no_producer_exists(self) -> None:
        self.create()
        self.transition(
            {
                "local": local_green(),
                "github": {
                    "state": "pending",
                    "run_ids": [],
                    "jobs": [],
                },
            }
        )
        seeded = obligations.get_record("test-obligation", self.store)
        self.assertFalse(protocol._verification_in_flight(seeded))
        self.assertTrue(protocol._verification_state_needs_reconcile(seeded))
        protocol.evaluate_obligation("test-obligation", store_path=self.store)
        with mock.patch.object(protocol, "github_runs", return_value=[]):
            with redirect_stdout(io.StringIO()):
                self.assertEqual(
                    protocol.watch(
                        store_path=self.store,
                        obligation_id=None,
                        once=True,
                        poll_seconds=1,
                    ),
                    0,
                )
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["overall_state"], "satisfied")
        self.assertEqual(record["github"]["state"], "no_result")
        self.assertFalse(protocol._verification_in_flight(record))
        self.assertTrue(protocol._watch_complete(record))

    def test_hosted_red_stays_loud_but_cannot_dispatch_remediation(
        self,
    ) -> None:
        self.canonical_receipt_no_result()
        # A/B: every hosted failure class remains a loud remediation obligation,
        # but Hermit's demoted hosted leg cannot authorize a mutation of main.
        red_conclusions = ("failure", "timed_out", "error", "startup_failure")
        self.assertEqual(len(red_conclusions), 4)
        self.assertEqual(set(red_conclusions), set(protocol._RED_CONCLUSIONS))
        warned_without_dispatch = 0
        for index, conclusion in enumerate(red_conclusions):
            store = self.root / f"positive-{index}.jsonl"
            obligation_id = f"positive-{index}"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="speculative",
                actor="test",
                obligation_id=obligation_id,
                path=store,
            )
            # The hosted leg reports a genuine red via the real classifier, so this
            # is not a hand-forced "red" string but the taxonomy value _github_state
            # derives from an authoritative failing conclusion.
            self.assertEqual(
                protocol._github_state(
                    {"status": "completed", "conclusion": conclusion}
                ),
                "red",
            )
            obligations.transition(
                obligation_id,
                "github-red",
                {
                    "local": {"state": "no_result"},
                    "github": {
                        "state": "red",
                        "finished_at": "2026-08-04T03:00:00Z",
                        "workflow_name": protocol.DEFAULT_WORKFLOW,
                        "urls": ["https://example/run"],
                    },
                },
                store,
            )
            with redirect_stderr(io.StringIO()):
                record = protocol.evaluate_obligation(
                    obligation_id, store_path=store, main_sha=SHA
                )
            self.assertEqual(record["overall_state"], "remediation_required", conclusion)
            self.assertEqual(record["recommendation"]["action"], "fix-forward", conclusion)
            self.assertEqual(
                record["recommendation"]["authority"], "advisory-only", conclusion
            )
            self.assertEqual(
                record["remediation"]["state"], "awaiting_local_authority", conclusion
            )
            self.assertIsNone(record["remediation"].get("dispatch"), conclusion)
            warned_without_dispatch += 1
        self.assertEqual(warned_without_dispatch, 4)

    def test_hosted_advisory_tombstones_a_legacy_revert_dispatch(self) -> None:
        self.canonical_receipt_no_result()
        self.create()
        self.transition(
            {
                "overall_state": "remediation_required",
                "local": {"state": "no_result"},
                "github": {"state": "red"},
                "failure_summary": "legacy hosted red",
                "recommendation": {"action": "revert"},
                "remediation": {
                    "state": "triggered",
                    "kind": "revert",
                    "dispatch": {
                        "state": "pending",
                        "instruction": "legacy unsafe revert instruction",
                    },
                },
            }
        )
        with redirect_stderr(io.StringIO()):
            migrated = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=SHA
            )
        self.assertEqual(
            migrated["remediation"]["state"], "awaiting_local_authority"
        )
        self.assertIsNone(migrated["remediation"]["dispatch"])
        self.assertEqual(migrated["recommendation"]["action"], "fix-forward")

    def test_no_result_is_distinct_from_both_pass_and_fail_for_a_local_red(
        self,
    ) -> None:
        # DISTINCTNESS bracket (task obligation_revert_path_lone): for the SAME
        # budget-spent local red, the hosted leg's terminal value selects THREE
        # different outcomes -- no_result must never be folded into either pass or
        # fail. This is the rc=2 UNVERIFIABLE vs rc=1 NOT-LANDED discipline applied
        # to verification legs.
        # The local ledger red is authoritative in all three cases. A hosted
        # answer changes neither that verdict nor the owner-only revert boundary.
        cases = {
            "green": ("remediation_required", "revert"),
            "no_result": ("remediation_required", "revert"),
            "red": ("remediation_required", "revert"),
        }
        outcomes = {}
        for github_state, (expected_overall, expected_action) in cases.items():
            store = self.root / f"distinct-{github_state}.jsonl"
            obligation_id = f"distinct-{github_state}"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="speculative",
                actor="test",
                obligation_id=obligation_id,
                path=store,
            )
            github_leg = {"state": github_state}
            if github_state in ("green", "red"):
                github_leg["finished_at"] = "2026-08-04T03:00:00Z"
            obligations.transition(
                obligation_id,
                "legs-report",
                {
                    "local": {
                        "state": "red",
                        "finished_at": "2026-08-04T03:04:59Z",
                        "exit_code": 1,
                        "log_path": "/tmp/local.log",
                        "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                    },
                    "github": github_leg,
                },
                store,
            )
            with redirect_stderr(io.StringIO()):
                record = protocol.evaluate_obligation(
                    obligation_id, store_path=store, main_sha=SHA
                )
            self.assertEqual(record["overall_state"], expected_overall, github_state)
            action = (record.get("recommendation") or {}).get("action")
            self.assertEqual(action, expected_action, github_state)
            self.assertEqual(record["remediation"]["state"], "awaiting_owner")
            self.assertIsNone(record["remediation"].get("dispatch"))
            outcomes[github_state] = (record["overall_state"], action)
        self.assertEqual(len(outcomes), 3)

    def test_failure_recommends_fix_forward_after_main_advances(self) -> None:
        self.create()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "finished_at": "2026-08-03T01:00:00Z",
                    "exit_code": 1,
                    "log_path": "/tmp/local.log",
                },
                "github": {"state": "no_result"},
            }
        )
        with redirect_stderr(io.StringIO()):
            record = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        self.assertEqual(record["recommendation"]["action"], "fix-forward")
        self.assertEqual(record["remediation"]["state"], "triggered")

    def test_completed_remediation_is_not_reopened_by_evaluation(self) -> None:
        """Historical red producers cannot resurrect a discharged obligation."""
        self.create()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "finished_at": "2026-08-03T01:00:00Z",
                    "exit_code": 1,
                    "log_path": "/tmp/local.log",
                },
                "github": {"state": "no_result"},
            }
        )
        with redirect_stderr(io.StringIO()):
            failed = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        self.assertEqual(failed["overall_state"], "remediation_required")
        self.assertEqual(failed["remediation"]["state"], "triggered")

        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=self.root),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
            mock.patch.object(protocol, "_resolve_raw_sha", side_effect=lambda _s, r: r),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=True),
        ):
            self.assertEqual(protocol.resolve_obligation(self.resolve_args()), 0)
        closed = obligations.get_record("test-obligation", self.store)
        self.assertEqual(closed["overall_state"], "remediated")
        self.assertEqual(closed["remediation"]["state"], "completed")
        before = self.store.read_bytes()

        with mock.patch.object(protocol, "bind_verification_policy") as bind_policy:
            reevaluated = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        bind_policy.assert_not_called()
        self.assertEqual(reevaluated, closed)
        self.assertEqual(self.store.read_bytes(), before)

    def test_hosted_advisory_tick_preserves_triggered_fix_forward(self) -> None:
        """A hosted advisory refresh cannot revoke an active repair exit."""
        self.canonical_receipt_no_result()
        self.create()
        triggered = self.transition(
            {
                "overall_state": "remediation_required",
                "local": {"state": "no_result"},
                "github": {
                    "state": "red",
                    "finished_at": "2026-08-10T01:00:00Z",
                    "workflow_name": protocol.DEFAULT_WORKFLOW,
                    "urls": ["https://example/run"],
                },
                "recommendation": {
                    "action": "fix-forward",
                    "authority": "advisory-only",
                },
                "remediation": {
                    "state": "triggered",
                    "kind": "fix-forward",
                    "dispatch": {"state": "acknowledged"},
                },
            }
        )
        with redirect_stderr(io.StringIO()):
            observed = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )

        self.assertEqual(observed["event_id"], triggered["event_id"])
        self.assertEqual(observed["remediation"]["state"], "triggered")
        self.assertEqual(observed["remediation"]["kind"], "fix-forward")

    def test_hosted_advisory_fix_forward_resolves_and_stays_completed(self) -> None:
        """A new advisory red opens loudly and has a durable repair exit."""
        self.canonical_receipt_no_result()
        self.create()
        self.transition(
            {
                "local": {"state": "no_result"},
                "github": {
                    "state": "red",
                    "finished_at": "2026-08-10T01:00:00Z",
                    "workflow_name": protocol.DEFAULT_WORKFLOW,
                    "urls": ["https://example/run"],
                },
            }
        )
        with redirect_stderr(io.StringIO()):
            opened = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        self.assertEqual(opened["overall_state"], "remediation_required")
        self.assertEqual(
            opened["remediation"]["state"], "awaiting_local_authority"
        )

        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=self.root),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
            mock.patch.object(protocol, "_resolve_raw_sha", side_effect=lambda _s, r: r),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=True),
        ):
            self.assertEqual(protocol.resolve_obligation(self.resolve_args()), 0)
        completed = obligations.get_record("test-obligation", self.store)
        self.assertEqual(completed["overall_state"], "remediated")
        self.assertEqual(completed["remediation"]["state"], "completed")

        reevaluated = protocol.evaluate_obligation(
            "test-obligation", store_path=self.store, main_sha=NEXT_SHA
        )
        self.assertEqual(reevaluated["event_id"], completed["event_id"])
        self.assertEqual(reevaluated["overall_state"], "remediated")

    def _running_watcher(
        self, *, start_ticks: int = 12345, terminal: bool = True
    ) -> dict:
        self.create()
        patch = {
            "watcher": {
                "state": "running",
                "pid": 4242,
                "launch_token": "watch-token",
                "started_at": "2026-08-08T23:00:00Z",
                "identity": {
                    "host": protocol.socket.gethostname(),
                    "boot_id": "test-boot",
                    "start_ticks": start_ticks,
                },
            }
        }
        if terminal:
            patch.update(
                {
                    "overall_state": "remediated",
                    "remediation": {
                        "state": "completed",
                        "kind": "fix-forward",
                        "ref": NEXT_SHA,
                    },
                }
            )
        return self.transition(patch)

    def test_cancel_watcher_uses_exact_pidfd_identity_and_preserves_obligation(
        self,
    ) -> None:
        self._running_watcher()
        args = argparse.Namespace(
            id="test-obligation",
            reason="operator-requested stuck watcher retirement",
            actor="test-operator",
            wait_seconds=1.0,
            store=self.store,
        )
        with (
            mock.patch.object(protocol, "_current_boot_id", return_value="test-boot"),
            mock.patch.object(protocol, "_process_start_ticks", return_value=12345),
            mock.patch.object(protocol.os, "pidfd_open", return_value=77) as pidfd_open,
            mock.patch.object(protocol.signal, "pidfd_send_signal") as pidfd_signal,
            mock.patch.object(protocol, "_wait_for_pidfd_exit", return_value=True),
            mock.patch.object(protocol.os, "close") as close,
            redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(protocol.cancel_watcher(args), 0)
        pidfd_open.assert_called_once_with(4242, 0)
        pidfd_signal.assert_called_once_with(77, protocol.signal.SIGTERM, None, 0)
        close.assert_called_once_with(77)
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["overall_state"], "remediated")
        self.assertEqual(record["watcher"]["state"], "cancelled")
        self.assertIsNone(record["watcher"]["pid"])
        self.assertEqual(record["watcher"]["identity"]["start_ticks"], 12345)
        self.assertEqual(record["watcher"]["cancellation"]["state"], "completed")

    def test_cancel_watcher_refuses_reused_pid_before_signalling(self) -> None:
        self._running_watcher(start_ticks=12345)
        args = argparse.Namespace(
            id="test-obligation",
            reason="must not signal a reused pid",
            actor="test-operator",
            wait_seconds=1.0,
            store=self.store,
        )
        before = obligations.get_record("test-obligation", self.store)
        with (
            mock.patch.object(protocol, "_current_boot_id", return_value="test-boot"),
            mock.patch.object(protocol, "_process_start_ticks", return_value=99999),
            mock.patch.object(protocol.os, "pidfd_open", return_value=77),
            mock.patch.object(protocol.signal, "pidfd_send_signal") as pidfd_signal,
            mock.patch.object(protocol.os, "close"),
            self.assertRaisesRegex(protocol.ProtocolError, "was reused"),
        ):
            protocol.cancel_watcher(args)
        pidfd_signal.assert_not_called()
        after = obligations.get_record("test-obligation", self.store)
        self.assertEqual(after["event_id"], before["event_id"])
        self.assertEqual(after["watcher"]["state"], "running")
        self.assertEqual(after["watcher"]["pid"], 4242)
        self.assertEqual(after["watcher"]["identity"]["start_ticks"], 12345)
        self.assertEqual(
            after["watcher"].get("cancellation"),
            before["watcher"].get("cancellation"),
        )

    def test_cancel_watcher_terminates_its_exact_child(self) -> None:
        child = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            start_new_session=True,
        )
        try:
            self.create()
            identity = protocol._watcher_process_identity(child.pid)
            self.transition(
                {
                    "overall_state": "remediated",
                    "remediation": {
                        "state": "completed",
                        "kind": "fix-forward",
                        "ref": NEXT_SHA,
                    },
                    "watcher": {
                        "state": "running",
                        "pid": child.pid,
                        "launch_token": "real-child-token",
                        "started_at": "2026-08-08T23:00:00Z",
                        "identity": identity,
                    },
                }
            )
            args = argparse.Namespace(
                id="test-obligation",
                reason="exercise exact child cancellation",
                actor="test-operator",
                wait_seconds=2.0,
                store=self.store,
            )
            with redirect_stdout(io.StringIO()):
                self.assertEqual(protocol.cancel_watcher(args), 0)
            self.assertEqual(child.wait(timeout=2), -protocol.signal.SIGTERM)
            record = obligations.get_record("test-obligation", self.store)
            self.assertEqual(record["watcher"]["state"], "cancelled")
            self.assertEqual(
                record["watcher"]["identity"]["start_ticks"],
                identity["start_ticks"],
            )
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=2)

    def test_cancel_watcher_never_escalates_after_sigterm_timeout(self) -> None:
        self._running_watcher()
        args = argparse.Namespace(
            id="test-obligation",
            reason="bounded signal timeout",
            actor="test-operator",
            wait_seconds=1.0,
            store=self.store,
        )
        with (
            mock.patch.object(protocol, "_current_boot_id", return_value="test-boot"),
            mock.patch.object(protocol, "_process_start_ticks", return_value=12345),
            mock.patch.object(protocol.os, "pidfd_open", return_value=77),
            mock.patch.object(protocol.signal, "pidfd_send_signal") as pidfd_signal,
            mock.patch.object(protocol, "_wait_for_pidfd_exit", return_value=False),
            mock.patch.object(protocol.os, "close"),
            self.assertRaisesRegex(protocol.ProtocolError, "no stronger signal"),
        ):
            protocol.cancel_watcher(args)
        pidfd_signal.assert_called_once_with(77, protocol.signal.SIGTERM, None, 0)
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["watcher"]["state"], "running")
        self.assertEqual(record["watcher"]["cancellation"]["state"], "timed_out")

    def test_cancel_watcher_refuses_an_open_obligation(self) -> None:
        self._running_watcher(terminal=False)
        args = argparse.Namespace(
            id="test-obligation",
            reason="must not suppress active verification",
            actor="test-operator",
            wait_seconds=1.0,
            store=self.store,
        )
        with (
            mock.patch.object(protocol.os, "pidfd_open") as pidfd_open,
            self.assertRaisesRegex(protocol.ProtocolError, "not terminal"),
        ):
            protocol.cancel_watcher(args)
        pidfd_open.assert_not_called()

    def test_resolve_refuses_wrong_state_before_repository_queries(self) -> None:
        before = self.create()
        with (
            mock.patch.object(protocol, "resolve_repo_source") as repo_source,
            self.assertRaisesRegex(protocol.ProtocolError, "remediation_required"),
        ):
            protocol.resolve_obligation(self.resolve_args())
        repo_source.assert_not_called()
        after = obligations.get_record("test-obligation", self.store)
        self.assertEqual(after["event_id"], before["event_id"])
        self.assertEqual(after["overall_state"], "open")
        self.assertEqual(after["remediation"]["state"], "none")

    def test_resolve_records_a_kind_that_differs_from_the_recommendation(
        self,
    ) -> None:
        """A recommendation is a prediction; the resolution is what was DONE.

        This used to raise 'contradicts durable recommendation', which left a
        genuinely discharged obligation with no honest resolution: refuse and
        corrupt the ledger by omission, or claim the recommended repair and
        corrupt it by commission. The disagreement is now RECORDED, not refused.
        """
        self.create()
        self.require_remediation("revert")
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=self.root),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
            mock.patch.object(protocol, "_resolve_raw_sha", side_effect=lambda _s, r: r),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=True),
            redirect_stderr(io.StringIO()) as captured,
        ):
            # kind=fix-forward against a revert recommendation.
            self.assertEqual(protocol.resolve_obligation(self.resolve_args()), 0)
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["overall_state"], "remediated")
        self.assertEqual(record["remediation"]["kind"], "fix-forward")
        verification = record["remediation"]["landing_verification"]["kind_verification"]
        # Both sides are on the record, and the disagreement is explicit rather
        # than something a reader has to infer.
        self.assertEqual(verification["kind"], "fix-forward")
        self.assertEqual(verification["recommended_kind"], "revert")
        self.assertFalse(verification["durable_recommendation_matches"])
        self.assertIn("differs from durable recommendation", captured.getvalue())

    def test_resolve_records_a_matching_kind_as_matching(self) -> None:
        """The unchanged path: prediction and reality agree, recorded as agreeing."""
        self.create()
        self.require_remediation("fix-forward")
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=self.root),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
            mock.patch.object(protocol, "_resolve_raw_sha", side_effect=lambda _s, r: r),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=True),
        ):
            self.assertEqual(protocol.resolve_obligation(self.resolve_args()), 0)
        record = obligations.get_record("test-obligation", self.store)
        verification = record["remediation"]["landing_verification"]["kind_verification"]
        self.assertEqual(verification["kind"], "fix-forward")
        self.assertEqual(verification["recommended_kind"], "fix-forward")
        self.assertTrue(verification["durable_recommendation_matches"])

    def test_resolve_refuses_an_unevidenced_revert_claim_on_the_new_path(
        self,
    ) -> None:
        """The door did NOT open: evidence keys on the CLAIM, not the guess.

        This is the newly-REACHABLE combination — claiming `revert` when
        `fix-forward` was recommended. The old code refused it for merely
        disagreeing with the recommendation and never looked at the trees; now
        that disagreement is allowed, so the tree proof is the ONLY thing
        standing between a `revert` claim and the ledger. Here the repair is an
        ordinary forward commit, so the claim must be refused on EVIDENCE.

        (The recommended-revert variant of this is already covered by
        `test_revert_resolution_requires_exact_failed_tree_restoration`.)
        """
        source = self.root / "unevidenced-revert"
        failed_land, repair = self._linear_repair_history(source)
        args = argparse.Namespace(
            id="unevidenced-revert",
            kind="revert",
            ref=repair,
            started_at=None,
            source=source,
            store=self.store,
        )
        self._arm(source, "unevidenced-revert", failed_land, recommended="fix-forward")
        before = obligations.get_record("unevidenced-revert", self.store)
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=source),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
            self.assertRaisesRegex(protocol.ProtocolError, "does not restore"),
        ):
            protocol.resolve_obligation(args)
        after = obligations.get_record("unevidenced-revert", self.store)
        self.assertEqual(after["event_id"], before["event_id"])
        self.assertEqual(after["overall_state"], "remediation_required")
        self.assertEqual(after["remediation"]["state"], "triggered")
        self.assertIsNone(after["remediation"].get("ref"))

    def test_resolve_refuses_an_unevidenced_fix_forward_claim(self) -> None:
        """The other half: a fix-forward whose ref is not a real repair.

        A commit that predates the failed land cannot be its repair, whatever
        kind is claimed and whatever was recommended.
        """
        source = self.root / "unevidenced-fixforward"
        failed_land, _repair = self._linear_repair_history(source)
        pre_failure = subprocess.run(
            ["git", "-C", str(source), "rev-parse", "HEAD~2"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        args = argparse.Namespace(
            id="unevidenced-fixforward",
            kind="fix-forward",
            ref=pre_failure,
            started_at=None,
            source=source,
            store=self.store,
        )
        self._arm(source, "unevidenced-fixforward", failed_land, recommended="revert")
        before = obligations.get_record("unevidenced-fixforward", self.store)
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=source),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
            self.assertRaisesRegex(protocol.ProtocolError, "does not descend"),
        ):
            protocol.resolve_obligation(args)
        after = obligations.get_record("unevidenced-fixforward", self.store)
        self.assertEqual(after["event_id"], before["event_id"])
        self.assertEqual(after["overall_state"], "remediation_required")
        self.assertEqual(after["remediation"]["state"], "triggered")
        self.assertIsNone(after["remediation"].get("ref"))

    def _linear_repair_history(self, source: Path) -> tuple[str, str]:
        """base -> failed_land -> repair, where repair is NOT a revert."""
        subprocess.run(["git", "init", "-q", "-b", "main", str(source)], check=True)
        for key, value in (("user.email", "test@example.com"), ("user.name", "Test")):
            subprocess.run(
                ["git", "-C", str(source), "config", key, value], check=True
            )

        def commit(name: str) -> str:
            (source / name).write_text(name)
            subprocess.run(["git", "-C", str(source), "add", name], check=True)
            subprocess.run(
                ["git", "-C", str(source), "commit", "-q", "-m", name], check=True
            )
            return subprocess.run(
                ["git", "-C", str(source), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()

        commit("base")
        failed_land = commit("failed")
        repair = commit("repair")
        return failed_land, repair

    def _arm(
        self, source: Path, obligation_id: str, landed_sha: str, *, recommended: str
    ) -> None:
        obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=landed_sha,
            land_mode="speculative",
            actor="test",
            obligation_id=obligation_id,
            path=self.store,
        )
        obligations.transition(
            obligation_id,
            "remediation-triggered",
            {
                "overall_state": "remediation_required",
                "recommendation": {"action": recommended},
                "remediation": {"state": "triggered", "kind": recommended},
            },
            self.store,
        )

    def test_resolve_refuses_nonexistent_repair(self) -> None:
        self.create()
        self.require_remediation()
        source = self.repo_source("rrnewton/hermit")
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol,
                "_resolve_raw_sha",
                side_effect=(
                    TREE,
                    protocol.ProtocolError("repair object does not exist"),
                ),
            ),
            self.assertRaisesRegex(protocol.ProtocolError, "does not exist"),
        ):
            protocol.resolve_obligation(self.resolve_args(source=source))
        self.assertEqual(
            obligations.get_record("test-obligation", self.store)["overall_state"],
            "remediation_required",
        )

    def test_resolve_refuses_source_from_wrong_repository(self) -> None:
        self.create()
        self.require_remediation()
        wrong_source = self.repo_source("rrnewton/reverie", "wrong-repo")
        with (
            mock.patch.object(protocol, "_fetch_target") as fetch_target,
            self.assertRaisesRegex(protocol.ProtocolError, "not required repository"),
        ):
            protocol.resolve_obligation(self.resolve_args(source=wrong_source))
        fetch_target.assert_not_called()
        self.assertEqual(
            obligations.get_record("test-obligation", self.store)["overall_state"],
            "remediation_required",
        )

    def test_resolve_refuses_repair_not_reachable_from_fresh_main(self) -> None:
        self.create()
        self.require_remediation()
        source = self.repo_source("rrnewton/hermit")
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol, "_resolve_raw_sha", side_effect=(TREE, NEXT_SHA)
            ),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=False),
            self.assertRaisesRegex(protocol.ProtocolError, "not reachable"),
        ):
            protocol.resolve_obligation(self.resolve_args(source=source))
        self.assertEqual(
            obligations.get_record("test-obligation", self.store)["overall_state"],
            "remediation_required",
        )

    def test_resolve_records_fresh_repository_main_ancestry_proof(self) -> None:
        self.create()
        self.require_remediation()
        source = self.repo_source("rrnewton/hermit")
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol, "_resolve_raw_sha", side_effect=(TREE, NEXT_SHA)
            ),
            mock.patch.object(
                protocol, "_is_target_ancestor", side_effect=(True, True)
            ),
        ):
            self.assertEqual(
                protocol.resolve_obligation(self.resolve_args(source=source)), 0
            )
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["overall_state"], "remediated")
        self.assertEqual(record["remediation"]["ref"], NEXT_SHA)
        proof = record["remediation"]["landing_verification"]
        self.assertEqual(proof["authority"], "fresh-repository-main-ancestry-v1")
        self.assertEqual(proof["repo"], "rrnewton/hermit")
        self.assertEqual(proof["repair_sha"], NEXT_SHA)
        self.assertEqual(proof["target_ref"], "origin/main")
        self.assertEqual(proof["target_tip_sha"], TREE)
        self.assertIs(proof["repair_is_ancestor_of_target_tip"], True)
        self.assertEqual(proof["failed_land_sha"], SHA)
        self.assertIs(proof["failed_land_is_ancestor_of_repair"], True)
        self.assertEqual(
            proof["kind_verification"],
            {
                "kind": "fix-forward",
                # Recorded alongside the actual kind so a reader can see whether
                # the prediction held, instead of inferring it from its absence.
                "recommended_kind": "fix-forward",
                "durable_recommendation_matches": True,
            },
        )

    def test_resolve_refuses_pre_failure_main_ancestor_as_repair(self) -> None:
        self.create()
        self.require_remediation()
        source = self.repo_source("rrnewton/hermit")
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol, "_resolve_raw_sha", side_effect=(TREE, NEXT_SHA)
            ),
            mock.patch.object(
                protocol, "_is_target_ancestor", side_effect=(True, False)
            ),
            self.assertRaisesRegex(protocol.ProtocolError, "does not descend"),
        ):
            protocol.resolve_obligation(self.resolve_args(source=source))
        self.assertEqual(
            obligations.get_record("test-obligation", self.store)["overall_state"],
            "remediation_required",
        )

    def test_resolve_cas_refuses_concurrent_state_change(self) -> None:
        self.create()
        self.require_remediation()
        source = self.repo_source("rrnewton/hermit")
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol, "_resolve_raw_sha", side_effect=(TREE, NEXT_SHA)
            ),
            mock.patch.object(
                protocol, "_is_target_ancestor", side_effect=(True, True)
            ),
            mock.patch.object(
                obligations, "transition_if_matches", return_value=None
            ),
            self.assertRaisesRegex(protocol.ProtocolError, "changed while"),
        ):
            protocol.resolve_obligation(self.resolve_args(source=source))

    def test_resolve_cas_accepts_concurrent_advisory_refresh(self) -> None:
        """Advisory evidence/substate churn cannot defeat unchanged authority."""
        self.create()
        self.require_remediation()
        source = self.repo_source("rrnewton/hermit")
        original_transition = obligations.transition_if_matches

        def refresh_then_compare(*args, **kwargs):
            obligations.transition(
                "test-obligation",
                "hosted-remediation-advisory-raced",
                {
                    "github": {"last_poll_error": "same semantic verdict, new poll"},
                    "remediation": {
                        "state": "awaiting_local_authority",
                        "kind": "fix-forward",
                        "dispatch": None,
                    },
                },
                self.store,
            )
            return original_transition(*args, **kwargs)

        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol, "_resolve_raw_sha", side_effect=(TREE, NEXT_SHA)
            ),
            mock.patch.object(
                protocol, "_is_target_ancestor", side_effect=(True, True)
            ),
            mock.patch.object(
                obligations,
                "transition_if_matches",
                side_effect=refresh_then_compare,
            ),
        ):
            self.assertEqual(
                protocol.resolve_obligation(self.resolve_args(source=source)), 0
            )
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["overall_state"], "remediated")
        self.assertEqual(record["remediation"]["state"], "completed")

    def test_resolve_cas_refuses_concurrent_same_state_action_change(self) -> None:
        self.create()
        self.require_remediation()
        source = self.repo_source("rrnewton/hermit")
        original_transition = obligations.transition_if_matches

        def race_then_compare(*args, **kwargs):
            obligations.transition(
                "test-obligation",
                "remediation-action-raced",
                {
                    "overall_state": "remediation_required",
                    "recommendation": {"action": "revert"},
                    "remediation": {"state": "triggered", "kind": "revert"},
                },
                self.store,
            )
            return original_transition(*args, **kwargs)

        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(
                protocol, "_resolve_raw_sha", side_effect=(TREE, NEXT_SHA)
            ),
            mock.patch.object(
                protocol, "_is_target_ancestor", side_effect=(True, True)
            ),
            mock.patch.object(
                obligations,
                "transition_if_matches",
                side_effect=race_then_compare,
            ),
            self.assertRaisesRegex(protocol.ProtocolError, "changed while"),
        ):
            protocol.resolve_obligation(self.resolve_args(source=source))
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["overall_state"], "remediation_required")
        self.assertEqual(record["recommendation"]["action"], "revert")

    def test_revert_resolution_requires_exact_failed_tree_restoration(self) -> None:
        source = self.root / "revert-history"
        subprocess.run(["git", "init", "-q", "-b", "main", str(source)], check=True)
        subprocess.run(
            ["git", "-C", str(source), "config", "user.email", "test@example.com"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(source), "config", "user.name", "Test"],
            check=True,
        )

        tracked = source / "tracked"
        tracked.write_text("healthy\n")
        subprocess.run(["git", "-C", str(source), "add", "tracked"], check=True)
        subprocess.run(
            ["git", "-C", str(source), "commit", "-q", "-m", "base"],
            check=True,
        )
        base = subprocess.run(
            ["git", "-C", str(source), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        tracked.write_text("broken\n")
        subprocess.run(["git", "-C", str(source), "add", "tracked"], check=True)
        subprocess.run(
            ["git", "-C", str(source), "commit", "-q", "-m", "failed land"],
            check=True,
        )
        failed_land = subprocess.run(
            ["git", "-C", str(source), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

        subprocess.run(
            ["git", "-C", str(source), "switch", "-q", "-c", "fake-revert"],
            check=True,
        )
        (source / "unrelated").write_text("not a revert\n")
        subprocess.run(["git", "-C", str(source), "add", "unrelated"], check=True)
        subprocess.run(
            ["git", "-C", str(source), "commit", "-q", "-m", "fake revert"],
            check=True,
        )
        fake_revert = subprocess.run(
            ["git", "-C", str(source), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        subprocess.run(
            ["git", "-C", str(source), "switch", "-q", "main"], check=True
        )
        subprocess.run(
            ["git", "-C", str(source), "revert", "--no-edit", failed_land],
            check=True,
            capture_output=True,
            text=True,
        )
        real_revert = subprocess.run(
            ["git", "-C", str(source), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

        obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=failed_land,
            land_mode="speculative",
            actor="test",
            obligation_id="revert-authority",
            path=self.store,
        )
        obligations.transition(
            "revert-authority",
            "remediation-triggered",
            {
                "overall_state": "remediation_required",
                "recommendation": {"action": "revert"},
                "remediation": {"state": "triggered", "kind": "revert"},
            },
            self.store,
        )
        args = argparse.Namespace(
            id="revert-authority",
            kind="revert",
            ref=fake_revert,
            started_at=None,
            source=source,
            store=self.store,
        )
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=source),
            mock.patch.object(protocol, "_fetch_target", return_value="fake-revert"),
            self.assertRaisesRegex(protocol.ProtocolError, "does not restore"),
        ):
            protocol.resolve_obligation(args)
        self.assertEqual(
            obligations.get_record("revert-authority", self.store)["overall_state"],
            "remediation_required",
        )

        args.ref = real_revert
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=source),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
        ):
            self.assertEqual(protocol.resolve_obligation(args), 0)
        proof = obligations.get_record("revert-authority", self.store)[
            "remediation"
        ]["landing_verification"]
        kind_proof = proof["kind_verification"]
        self.assertEqual(kind_proof["failed_land_parent_sha"], base)
        self.assertEqual(kind_proof["repair_parent_sha"], failed_land)
        self.assertIs(kind_proof["tree_restored"], True)
        self.assertEqual(
            kind_proof["repair_tree_sha"], kind_proof["failed_land_parent_tree_sha"]
        )

    def test_resolve_uses_real_git_object_and_lineage_authority(self) -> None:
        source = self.root / "repair-history"
        subprocess.run(["git", "init", "-q", "-b", "main", str(source)], check=True)
        subprocess.run(
            ["git", "-C", str(source), "config", "user.email", "test@example.com"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(source), "config", "user.name", "Test"],
            check=True,
        )

        def commit(name: str, body: str) -> str:
            path = source / name
            path.write_text(body)
            subprocess.run(["git", "-C", str(source), "add", name], check=True)
            subprocess.run(
                ["git", "-C", str(source), "commit", "-q", "-m", name],
                check=True,
            )
            return subprocess.run(
                ["git", "-C", str(source), "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()

        pre_failure = commit("base", "base")
        failed_land = commit("failed", "failed")
        repair = commit("repair", "repair")
        obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=failed_land,
            land_mode="speculative",
            actor="test",
            obligation_id="git-authority",
            path=self.store,
        )
        obligations.transition(
            "git-authority",
            "remediation-triggered",
            {
                "overall_state": "remediation_required",
                "recommendation": {"action": "fix-forward"},
                "remediation": {"state": "triggered", "kind": "fix-forward"},
            },
            self.store,
        )

        args = argparse.Namespace(
            id="git-authority",
            kind="fix-forward",
            ref=pre_failure,
            started_at=None,
            source=source,
            store=self.store,
        )
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=source),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
            self.assertRaisesRegex(protocol.ProtocolError, "does not descend"),
        ):
            protocol.resolve_obligation(args)

        args.ref = repair
        with (
            mock.patch.object(protocol, "resolve_repo_source", return_value=source),
            mock.patch.object(protocol, "_fetch_target", return_value="main"),
        ):
            self.assertEqual(protocol.resolve_obligation(args), 0)
        record = obligations.get_record("git-authority", self.store)
        proof = record["remediation"]["landing_verification"]
        self.assertEqual(proof["repair_sha"], repair)
        self.assertEqual(proof["target_tip_sha"], repair)
        self.assertEqual(proof["failed_land_sha"], failed_land)

    def test_remediation_trigger_is_idempotent(self) -> None:
        self.create()
        self.transition({"local": {"state": "red"}, "github": {"state": "no_result"}})
        with redirect_stderr(io.StringIO()):
            first = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
            second = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        self.assertEqual(first["event_id"], second["event_id"])
        events = [json.loads(line) for line in self.store.read_text().splitlines()]
        self.assertEqual(
            sum(event["event_type"] == "remediation-triggered" for event in events), 1
        )

    def test_stale_trigger_cannot_reopen_completed_resolution(self) -> None:
        """A watcher snapshot taken before completion loses to the operator."""
        self.create()
        stale = self.transition(
            {
                "overall_state": "remediation_required",
                "recommendation": {"action": "fix-forward"},
                "remediation": {
                    "state": "recommended",
                    "kind": "fix-forward",
                    "dispatch": None,
                },
            }
        )
        completed = self.transition(
            {
                "overall_state": "remediated",
                "remediation": {
                    "state": "completed",
                    "kind": "fix-forward",
                    "ref": NEXT_SHA,
                },
            }
        )

        observed = protocol.trigger_remediation(stale, store_path=self.store)

        self.assertEqual(observed["event_id"], completed["event_id"])
        self.assertEqual(observed["overall_state"], "remediated")
        self.assertEqual(observed["remediation"]["state"], "completed")

    def test_wake_delivery_is_unhandled_until_reader_acknowledges(self) -> None:
        self.create()
        self.transition({"local": {"state": "red"}, "github": {"state": "no_result"}})
        with redirect_stderr(io.StringIO()):
            protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        with redirect_stdout(io.StringIO()):
            protocol.record_wake_sent(
                store_path=self.store, target="hermit-lander", source="test-orc"
            )
        sent = obligations.get_record("test-obligation", self.store)
        dispatch = sent["remediation"]["dispatch"]
        self.assertEqual(dispatch["state"], "sent_unacknowledged")
        self.assertEqual(dispatch["wake_attempt"], 1)
        self.assertIsNone(dispatch["acknowledged_at"])

        with redirect_stdout(io.StringIO()):
            first = protocol.inherit_actionable(
                store_path=self.store, agent="hermit-lander", session="replacement-1"
            )
            second = protocol.inherit_actionable(
                store_path=self.store, agent="hermit-lander", session="replacement-1"
            )
        self.assertEqual(len(first), 1)
        self.assertEqual(len(second), 1)
        acknowledged = obligations.get_record("test-obligation", self.store)
        dispatch = acknowledged["remediation"]["dispatch"]
        self.assertEqual(dispatch["state"], "acknowledged")
        self.assertEqual(dispatch["acknowledged_by"], "hermit-lander")
        self.assertEqual(dispatch["acknowledged_session"], "replacement-1")
        self.assertNotIn("acknowledgement", dispatch)
        events = [json.loads(line) for line in self.store.read_text().splitlines()]
        self.assertEqual(
            sum(event["event_type"] == "remediation-inherited" for event in events), 1
        )

    def test_executed_acknowledgement_succeeds_without_reason(self) -> None:
        self.create()
        self.transition({"local": {"state": "red"}, "github": {"state": "no_result"}})
        with redirect_stderr(io.StringIO()):
            protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        with redirect_stdout(io.StringIO()):
            protocol.inherit_actionable(
                store_path=self.store,
                agent="hermit-lander",
                session="execution-session",
                action_outcome="executed",
            )
        acknowledgement = obligations.get_record("test-obligation", self.store)[
            "remediation"
        ]["dispatch"]["acknowledgement"]
        self.assertEqual(acknowledgement["outcome"], "executed")
        self.assertIsNone(acknowledgement["reason"])

    def test_declined_acknowledgement_without_reason_is_refused(self) -> None:
        self.create()
        self.transition({"local": {"state": "red"}, "github": {"state": "no_result"}})
        with redirect_stderr(io.StringIO()):
            protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        with self.assertRaisesRegex(protocol.ProtocolError, "reason is required"):
            protocol.inherit_actionable(
                store_path=self.store,
                agent="hermit-lander",
                session="refusal-session",
                action_outcome="declined",
            )
        dispatch = obligations.get_record("test-obligation", self.store)[
            "remediation"
        ]["dispatch"]
        self.assertNotEqual(dispatch["state"], "acknowledged")
        self.assertNotIn("acknowledgement", dispatch)

    def test_declined_acknowledgement_stores_retrievable_reason(self) -> None:
        self.create()
        self.transition({"local": {"state": "red"}, "github": {"state": "no_result"}})
        with redirect_stderr(io.StringIO()):
            protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        reason = "hosted red predates the landed change and would survive revert"
        with redirect_stdout(io.StringIO()):
            protocol.inherit_actionable(
                store_path=self.store,
                agent="hermit-lander",
                session="refusal-session",
                action_outcome="declined",
                reason=reason,
            )
        record = obligations.get_record("test-obligation", self.store)
        acknowledgement = record["remediation"]["dispatch"]["acknowledgement"]
        self.assertEqual(acknowledgement["outcome"], "declined")
        self.assertEqual(acknowledgement["reason"], reason)
        self.assertEqual(acknowledgement["recorded_by"], "hermit-lander")
        self.assertEqual(acknowledgement["recorded_session"], "refusal-session")
        self.assertEqual(record["overall_state"], "remediation_required")

        # A fresh reader's receipt must not erase the earlier refusal rationale.
        with redirect_stdout(io.StringIO()):
            protocol.inherit_actionable(
                store_path=self.store,
                agent="replacement-lander",
                session="fresh-session",
            )
        acknowledgement = obligations.get_record("test-obligation", self.store)[
            "remediation"
        ]["dispatch"]["acknowledgement"]
        self.assertEqual(acknowledgement["outcome"], "declined")
        self.assertEqual(acknowledgement["reason"], reason)

    def test_new_lander_session_rediscovers_acknowledged_obligation(self) -> None:
        self.create()
        self.transition({"local": {"state": "red"}, "github": {"state": "no_result"}})
        with redirect_stderr(io.StringIO()):
            protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=NEXT_SHA
            )
        with redirect_stdout(io.StringIO()):
            protocol.inherit_actionable(
                store_path=self.store, agent="hermit-lander", session="old-session"
            )
            protocol.inherit_actionable(
                store_path=self.store, agent="hermit-lander", session="fresh-session"
            )
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(
            record["remediation"]["dispatch"]["acknowledged_session"],
            "fresh-session",
        )
        self.assertEqual(record["overall_state"], "remediation_required")

        # A later advisory wake must not turn already-handled durable work back
        # into an unhandled state.
        with redirect_stdout(io.StringIO()):
            protocol.record_wake_sent(
                store_path=self.store, target="hermit-lander", source="late-orc"
            )
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["remediation"]["dispatch"]["state"], "acknowledged")
        self.assertEqual(
            record["remediation"]["dispatch"]["acknowledged_session"],
            "fresh-session",
        )

    def test_either_green_satisfies_without_waiting_for_nonred_peer(self) -> None:
        cases = (
            ("green", "pending"),
            ("green", "running"),
            ("green", "no_result"),
            ("green", "green"),
            ("pending", "green"),
            ("running", "green"),
            ("no_result", "green"),
        )
        self.assertEqual(len(cases), 7)
        for index, states in enumerate(cases):
            store = self.root / f"green-or-{index}.jsonl"
            obligation_id = f"green-or-{index}"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="speculative",
                verification_policy=protocol.verification_policy_for_repo(
                    "rrnewton/hermit"
                ),
                actor="test",
                obligation_id=obligation_id,
                path=store,
            )
            obligations.transition(
                obligation_id,
                "legs",
                {
                    "local": (
                        local_green() if states[0] == "green" else {"state": states[0]}
                    ),
                    "github": {"state": states[1]},
                },
                store,
            )
            with mock.patch.object(protocol, "trigger_remediation") as actuator:
                record = protocol.evaluate_obligation(
                    obligation_id, store_path=store, main_sha=SHA
                )
            actuator.assert_not_called()
            self.assertEqual(record["overall_state"], "satisfied", states)
            self.assertIsNotNone(record["satisfied_at"])

    def test_health_codes_distinguish_open_and_remediation(self) -> None:
        self.create()
        # A FRESH open obligation is the normal post-merge window and must be
        # QUIET (rc 0). Paging here is what made this gate fire on every land.
        with redirect_stdout(io.StringIO()):
            self.assertEqual(
                protocol.print_status(
                    self.store,
                    include_closed=False,
                    json_output=False,
                    gate=True,
                    actionable_only=False,
                ),
                0,
            )
        # The SAME open obligation, once it has outlived its bound, is rc 1.
        # Driven by shrinking the bound rather than by faking a timestamp, so the
        # obligation under test is byte-identical in both assertions and only the
        # age judgement differs.
        with redirect_stdout(io.StringIO()), mock.patch.object(
            protocol, "OBLIGATION_STALE_AFTER_SECS", 0.0
        ):
            self.assertEqual(
                protocol.print_status(
                    self.store,
                    include_closed=False,
                    json_output=False,
                    gate=True,
                    actionable_only=False,
                ),
                1,
            )
        self.transition({"github": {"state": "red"}})
        with redirect_stderr(io.StringIO()):
            protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=SHA
            )
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(
                protocol.print_status(
                    self.store,
                    include_closed=False,
                    json_output=False,
                    gate=True,
                    actionable_only=True,
                ),
                2,
            )
        self.assertIn("state=remediation-required", output.getvalue())
        self.assertIn("sent_unacknowledged_count=0", output.getvalue())

    def test_empty_one_shot_watch_reports_a_domain_result(self) -> None:
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(
                protocol.watch(
                    store_path=self.store,
                    obligation_id=None,
                    once=True,
                    poll_seconds=1,
                ),
                0,
            )
        printed = output.getvalue()
        # The human-readable domain result is unchanged...
        self.assertIn(
            "WATCH OBLIGATIONS: checked=0 unresolved=0 remediation_required=0", printed
        )
        # ...and it now travels with the typed completeness fields, so a
        # consumer can tell "swept everything and found nothing" from "never
        # finished the sweep". Absence of findings is not evidence of health.
        self.assertIn("watch_status=complete", printed)
        self.assertIn("watch_timed_out=false", printed)
        self.assertIn("watch_planned=0", printed)
        self.assertIn("watch_checked=0", printed)
        self.assertIn("watch_verdict=CLEAR", printed)
        self.assertNotIn("NO-RESULT", printed)

    # ---------------------------------------------------------------- budget
    #
    # The gate used to be SIGKILLed by tick-hub's 30s guillotine mid-sweep,
    # which discarded ALL captured output -- so a tick where the sweep never
    # finished was indistinguishable from one that swept clean. These bracket
    # the wall budget in BOTH directions: it must fire and preserve partials,
    # and it must NOT fire when the sweep fits.

    def _budget_fixture(self, count: int, seconds_per_poll: float):
        """A fake clock advanced by each poll, so the budget is deterministic.

        Advancing the clock from inside the mocked poll (rather than asserting
        on a fixed sequence of monotonic() calls) keeps the test robust to
        refactors of where the timing calls sit.
        """
        clock = {"t": 0.0}
        records = [
            {
                "obligation_id": f"ob-{i}",
                "repo": "rrnewton/hermit",
                "landed_sha": f"{i}" * 40,
                "overall_state": "open",
                "local": {"state": "green"},
                "github": {"state": "no_result"},
                "recommendation": {},
                "remediation": {},
            }
            for i in range(count)
        ]

        def fake_poll(
            obligation_id: str,
            store_path: Path,
            *,
            deadline: float | None = None,
        ) -> dict:
            del deadline
            clock["t"] += seconds_per_poll
            return next(r for r in records if r["obligation_id"] == obligation_id)

        return clock, records, fake_poll

    def test_one_shot_watch_stops_on_budget_and_keeps_partial_results(self) -> None:
        clock, records, fake_poll = self._budget_fixture(5, seconds_per_poll=10.0)
        output = io.StringIO()
        with mock.patch.object(protocol.time, "monotonic", lambda: clock["t"]), \
             mock.patch.object(
                 protocol.obligations,
                 "latest_records",
                 return_value={record["obligation_id"]: record for record in records},
             ), \
             mock.patch.object(protocol, "poll_obligation", fake_poll), \
             redirect_stdout(output):
            rc = protocol.watch(
                store_path=self.store,
                obligation_id=None,
                once=True,
                poll_seconds=1,
                budget_secs=25.0,
            )
        printed = output.getvalue()
        # 0s, 10s, 20s are all under 25s; the fourth check at 30s is not.
        self.assertIn("watch_status=no-result", printed)
        self.assertIn("watch_verdict=NO-RESULT", printed)
        self.assertIn("state=no-result", printed)
        self.assertIn("watch_timed_out=true", printed)
        self.assertIn("watch_planned=5", printed)
        self.assertIn("watch_checked=3", printed)
        self.assertIn("elapsed_ms=30000", printed)
        self.assertIn("bound_ms=25000", printed)
        # THE POINT: the three already-polled obligations survive. Under the
        # old SIGKILL path every one of them was lost.
        for i in range(3):
            self.assertIn(f"ob-{i}", printed)
        # And an unfinished sweep is a distinct unavailable answer: neither
        # success (0) nor an ordinary completed-open tick (1).
        self.assertEqual(rc, protocol.WATCH_EXIT_NO_RESULT)

    def test_only_blocking_poll_crossing_bound_is_no_result(self) -> None:
        """An inner timeout leaves reporting time inside the unchanged bound."""
        clock, records, _ = self._budget_fixture(1, seconds_per_poll=0.0)

        def exhaust_budget(
            obligation_id: str,
            store_path: Path,
            *,
            deadline: float | None = None,
        ) -> dict:
            self.assertEqual(obligation_id, "ob-0")
            self.assertEqual(store_path, self.store)
            self.assertIsNotNone(deadline)
            clock["t"] = deadline - protocol.WATCH_GATE_COMPLETION_RESERVE_SECS
            raise protocol.WatchBudgetExhausted("planted inner timeout")

        output = io.StringIO()
        with mock.patch.object(protocol.time, "monotonic", lambda: clock["t"]), \
             mock.patch.object(
                 protocol.obligations,
                 "latest_records",
                 return_value={record["obligation_id"]: record for record in records},
             ), \
             mock.patch.object(protocol, "poll_obligation", exhaust_budget), \
             redirect_stdout(output):
            rc = protocol.watch(
                store_path=self.store,
                obligation_id=None,
                once=True,
                poll_seconds=1,
                budget_secs=25.0,
            )
        printed = output.getvalue()
        self.assertEqual(rc, protocol.WATCH_EXIT_NO_RESULT)
        self.assertIn("WATCH OBLIGATIONS: NO-RESULT", printed)
        self.assertIn("watch_checked=1", printed)
        self.assertIn("watch_planned=1", printed)
        self.assertIn("elapsed_ms=23000", printed)
        self.assertIn("bound_ms=25000", printed)
        self.assertLess(clock["t"], 25.0)
        self.assertNotIn("watch_status=complete", printed)
        self.assertNotIn("watch_verdict=CLEAR", printed)

    def test_one_shot_watch_that_fits_the_budget_reports_complete(self) -> None:
        """Positive control: the budget must not fire when the sweep fits."""
        clock, records, fake_poll = self._budget_fixture(3, seconds_per_poll=1.0)
        output = io.StringIO()
        with mock.patch.object(protocol.time, "monotonic", lambda: clock["t"]), \
             mock.patch.object(
                 protocol.obligations,
                 "latest_records",
                 return_value={record["obligation_id"]: record for record in records},
             ), \
             mock.patch.object(protocol, "poll_obligation", fake_poll), \
             redirect_stdout(output):
            protocol.watch(
                store_path=self.store,
                obligation_id=None,
                once=True,
                poll_seconds=1,
                budget_secs=25.0,
            )
        printed = output.getvalue()
        self.assertIn("watch_status=complete", printed)
        self.assertIn("watch_timed_out=false", printed)
        self.assertIn("watch_checked=3", printed)
        self.assertIn("watch_verdict=OPEN", printed)
        self.assertNotIn("NO-RESULT", printed)

    def test_gate_does_not_launder_watch_no_result_through_status(self) -> None:
        """The --gate adapter must preserve the watcher's typed refusal."""
        with mock.patch.object(
            protocol, "watch", return_value=protocol.WATCH_EXIT_NO_RESULT
        ), mock.patch.object(protocol, "print_status") as print_status:
            rc = protocol.main(
                ["watch", "--once", "--gate", "--store", str(self.store)]
            )
        self.assertEqual(rc, protocol.WATCH_EXIT_NO_RESULT)
        print_status.assert_not_called()

    def test_gate_normal_completion_still_reports_clear(self) -> None:
        output = io.StringIO()
        with redirect_stdout(output):
            rc = protocol.main(
                ["watch", "--once", "--gate", "--store", str(self.store)]
            )
        printed = output.getvalue()
        self.assertEqual(rc, 0)
        self.assertIn("watch_verdict=CLEAR", printed)
        self.assertIn("state=clear", printed)
        self.assertNotIn("NO-RESULT", printed)

    def test_per_poll_wall_and_cpu_are_reported_as_comment_lines(self) -> None:
        """The timing basis must ride along without polluting the gate fields.

        tick-hub's parse_kv_lines ignores '#' lines, so these are visible to a
        human reading captured output but contribute no key/value fields.
        """
        clock, records, fake_poll = self._budget_fixture(2, seconds_per_poll=1.0)
        output = io.StringIO()
        with mock.patch.object(protocol.time, "monotonic", lambda: clock["t"]), \
             mock.patch.object(
                 protocol.obligations,
                 "latest_records",
                 return_value={record["obligation_id"]: record for record in records},
             ), \
             mock.patch.object(protocol, "poll_obligation", fake_poll), \
             redirect_stdout(output):
            protocol.watch(
                store_path=self.store,
                obligation_id=None,
                once=True,
                poll_seconds=1,
                budget_secs=100.0,
            )
        timing = [
            line
            for line in output.getvalue().splitlines()
            if line.startswith("# poll ")
        ]
        self.assertEqual(len(timing), 2)
        for line in timing:
            self.assertIn("wall=", line)
            self.assertIn("cpu=", line)

    def test_canonical_local_receipt_positive_and_fail_closed_negatives(self) -> None:
        valid = counted_receipt_report()
        with mock.patch.object(
            protocol,
            "_run",
            return_value=subprocess.CompletedProcess([], 0, json.dumps(valid), ""),
        ):
            accepted, evidence = protocol.verify_local_receipt("rrnewton/hermit", SHA)
        self.assertTrue(accepted)
        self.assertEqual(evidence["state"], "verified")
        self.assertEqual(evidence["report"]["qualifying_count"], 1)
        self.assertNotIn("semantic_contract", evidence)
        self.assertEqual(
            evidence["report"]["newest_qualifying"]["receipt_identity"][
                "digest_algorithm"
            ],
            "sha256",
        )

        failed = failed_receipt_report()
        with mock.patch.object(
            protocol,
            "_run",
            return_value=subprocess.CompletedProcess([], 3, json.dumps(failed), ""),
        ):
            accepted, evidence = protocol.verify_local_receipt("rrnewton/hermit", SHA)
        self.assertFalse(accepted)
        self.assertEqual(evidence["state"], "failed")
        self.assertEqual(evidence["report"]["failed_record_count"], 1)

        negatives = {
            "bare-rc0": "",
            "malformed": "{not-json",
            "vacuous": json.dumps(
                {**valid, "qualifying_count": 0, "newest_qualifying": None}
            ),
            "wrong-sha": json.dumps({**valid, "sha": NEXT_SHA}),
            "partial-profile": json.dumps(
                {
                    **valid,
                    "newest_qualifying": {
                        **valid["newest_qualifying"],
                        "profile": "quick",
                    },
                }
            ),
            "schema4-fabricated-coverage": json.dumps(
                {
                    **valid,
                    "newest_qualifying": {
                        **valid["newest_qualifying"],
                        "coverage_satisfied": True,
                        "coverage_status": "satisfied",
                    },
                }
            ),
        }
        for name, output in negatives.items():
            with (
                self.subTest(name=name),
                mock.patch.object(
                    protocol,
                    "_run",
                    return_value=subprocess.CompletedProcess([], 0, output, ""),
                ),
            ):
                accepted, evidence = protocol.verify_local_receipt(
                    "rrnewton/hermit", SHA
                )
            self.assertFalse(accepted)
            self.assertEqual(evidence["state"], "refused")

        with mock.patch.object(
            protocol,
            "_run",
            return_value=subprocess.CompletedProcess([], 0, json.dumps(valid), ""),
        ):
            accepted, evidence = protocol.verify_local_receipt("rrnewton/reverie", SHA)
        self.assertFalse(accepted)
        self.assertIn("bound to rrnewton/hermit", evidence["reason"])

    def test_real_canonical_cli_brackets_planted_receipts(self) -> None:
        ledger = self.root / "ledger" / "hermit" / "test-host" / "2026-08.jsonl"
        observed_commands: list[tuple[str, ...]] = []
        producer_sha = json.loads(PRODUCER_REGISTRY.read_text())["registered_at"]
        subprocess.run(
            ["git", "-C", str(HERMIT), "cat-file", "-e", f"{producer_sha}^{{commit}}"],
            check=True,
            capture_output=True,
        )

        def planted_row() -> dict:
            row = ledger_receipt(producer_sha)
            row["cwd"] = str(HERMIT)
            return row

        def run_actual_cli(command, **kwargs):
            canonical_command = tuple(command)
            observed_commands.append(canonical_command)
            environment = os.environ.copy()
            environment.update(
                HERMIT_VALIDATE_STOP_TEST_MODE="1",
                CI_HUB_VALIDATE_LEDGER_TEST_ROOT=str(self.root),
            )
            return subprocess.run(
                canonical_command,
                check=False,
                text=True,
                capture_output=True,
                env=environment,
                timeout=kwargs.get("timeout"),
            )

        def verify(row: dict) -> tuple[bool, dict]:
            ledger.parent.mkdir(parents=True, exist_ok=True)
            event = protocol.canonical_ledger._event_for_row(row, self.root)
            ledger.write_text(json.dumps(event) + "\n", encoding="utf-8")
            with mock.patch.object(protocol, "_run", side_effect=run_actual_cli):
                return protocol.verify_local_receipt("rrnewton/hermit", producer_sha)

        accepted, evidence = verify(planted_row())
        self.assertTrue(accepted, evidence)
        self.assertEqual(evidence["state"], "verified")
        self.assertEqual(
            evidence["command"][:2],
            [str(protocol.LOCAL_RECEIPT_AUTHORITY), "validate-status"],
        )
        self.assertEqual(
            evidence["report"]["newest_qualifying"]["discovered_tests"], 13
        )
        self.assertEqual(
            observed_commands[0],
            tuple(evidence["command"]),
        )
        self.assertNotIn("--ledger", evidence["command"])

        exploit = planted_row()
        exploit.update(
            schema_version=5,
            repo="hermit",
            executed_tests=1,
            filtered_tests=0,
            failures=7,
            checks=0,
            gates_run=0,
            gates_expected=5,
            gates=[],
            coverage={
                "planned_test_nodes": 1,
                "executed_test_nodes": 1,
                "zero_executed_nodes": [],
                "absent_nodes": [],
            },
        )
        bad_gate_count = planted_row()
        bad_gate_count["checks"] = 1
        wrong_repo = planted_row()
        wrong_repo["repo"] = "reverie"
        schema5_without_coverage = planted_row()
        schema5_without_coverage["schema_version"] = 5
        schema5_without_coverage["repo"] = "hermit"
        schema5_without_repo = planted_row()
        schema5_without_repo["schema_version"] = 5
        schema5_without_repo["coverage"] = {
            "planned_test_nodes": 1,
            "executed_test_nodes": 1,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        }
        for name, planted in {
            "pass-with-failures-and-no-gates": exploit,
            "inconsistent-gate-count": bad_gate_count,
            "wrong-repository": wrong_repo,
            "schema5-without-coverage": schema5_without_coverage,
            "schema5-without-repository": schema5_without_repo,
        }.items():
            with self.subTest(name=name):
                accepted, evidence = verify(planted)
            self.assertFalse(accepted)
            self.assertEqual(evidence["state"], "refused")

    def test_bare_local_green_is_downgraded_without_counted_receipt(self) -> None:
        self.create()
        self.transition(
            {
                "local": {
                    "state": "green",
                    "exit_code": 0,
                    "receipt_verification": None,
                },
                "github": {"state": "no_result"},
            }
        )
        refused = {
            "state": "refused",
            "authority": "ci-hub-validate-status",
            "repo": "rrnewton/hermit",
            "reason": "canonical verifier reported no qualifying counted receipt",
        }
        self.current_local_receipt.side_effect = None
        self.current_local_receipt.return_value = (False, refused)
        record = protocol.evaluate_obligation("test-obligation", store_path=self.store)
        self.assertEqual(record["local"]["state"], "no_result")
        self.assertEqual(record["overall_state"], "open")

    def test_refused_receipt_downgrades_raw_red_without_remediation(self) -> None:
        self.create()
        refused = {
            "state": "refused",
            "authority": "ci-hub-validate-status",
            "repo": "rrnewton/hermit",
            "reason": "canonical verifier exited 4",
        }
        self.transition(
            {
                "local": {
                    "state": "red",
                    "exit_code": 1,
                    "classification_reason": "test-failure",
                    "receipt_verification": refused,
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                },
                "github": {"state": "no_result"},
            }
        )
        self.current_local_receipt.side_effect = None
        self.current_local_receipt.return_value = (False, refused)
        with mock.patch.object(protocol, "trigger_remediation") as actuator:
            record = protocol.evaluate_obligation(
                "test-obligation", store_path=self.store, main_sha=SHA
            )
        actuator.assert_not_called()
        self.assertEqual(record["local"]["state"], "no_result")
        self.assertEqual(record["overall_state"], "open")
        self.assertTrue(
            record["local"]["classification_reason"].startswith(
                "canonical-receipt-refused:"
            )
        )

    def test_canonical_failed_receipt_is_the_only_local_red_authority(self) -> None:
        self.create()
        failed = failed_local_receipt()
        self.transition(
            {
                "local": {
                    "state": "red",
                    "exit_code": 1,
                    "classification_reason": "test-failure",
                    "receipt_verification": failed,
                },
                "github": {"state": "running"},
            }
        )
        self.current_local_receipt.side_effect = None
        self.current_local_receipt.return_value = (False, failed)
        record = protocol.bind_local_receipt_authority(
            "test-obligation", self.store
        )
        self.assertEqual(record["local"]["state"], "red")
        self.assertEqual(record["local"]["receipt_verification"]["state"], "failed")

    def test_local_run_persists_tool_cost_payload(self) -> None:
        self.create()
        workspace = self.root / "ignored/ci-hub/obligations/test-obligation"
        cost_path = workspace / "cost.json"
        log_path = workspace / "local.log"
        estimate = {
            "kind": "derived",
            "wall_seconds": 1800.0,
            "cpu_seconds": 7200.0,
            "basis": "derived from test fixture history, n=3",
        }
        self.transition(
            {
                "local": {
                    "state": "starting",
                    "launch_token": "test-local-run-token",
                    "started_at": "2026-08-05T00:09:00Z",
                    "log_path": str(log_path),
                    "cost": {
                        "estimate": estimate,
                        "actual": None,
                        "record_path": str(cost_path),
                    },
                }
            }
        )

        validate_commands = []

        def fake_run(command, **_kwargs):
            command = list(command)
            if command[:2] == ["git", "clone"]:
                checkout = (
                    self.root / "ignored/ci-hub/obligations/test-obligation/hermit"
                )
                (checkout / "scripts").mkdir(parents=True)
                # The capability probe execs this path, so it must exist.
                (checkout / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
            # POSITIVE BRACKET: this driver ADVERTISES --no-label-pr, so the
            # producer must still hand it over. A probe that always omitted the
            # flag would pass the negative case while silently dropping
            # behaviour, so this direction is asserted too.
            if command[-1] == "--help" and command[0].endswith("/scripts/validate.rs"):
                return subprocess.CompletedProcess(
                    command,
                    0,
                    stdout="usage: validate.rs <profile> [options]\n"
                    "  --label-pr       Publish a receipt and label the PR (default).\n"
                    "  --no-label-pr    Disable the receipt publication and label update.\n",
                    stderr="",
                )
            if "rev-parse" in command:
                return subprocess.CompletedProcess(
                    command, 0, stdout=SHA + "\n", stderr=""
                )
            if "tool-cost" in command[0]:
                validate_commands.append(command)
                payload = {
                    "schema_version": 1,
                    "tool": "speculative-land/local-validate",
                    "estimate": estimate,
                    "actual": {
                        "wall_seconds": 12.5,
                        "cpu_seconds": 20.0,
                        "cpu_user_seconds": 15.0,
                        "cpu_system_seconds": 5.0,
                        "exit": "0",
                    },
                }
                cost_path.parent.mkdir(parents=True, exist_ok=True)
                cost_path.write_text(json.dumps(payload))
            if "validate-status" in command:
                return subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=json.dumps(counted_receipt_report()),
                    stderr="",
                )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        source = self.root / "source"
        source.mkdir()
        with (
            mock.patch.object(protocol, "ROOT", self.root),
            mock.patch.object(protocol, "_run", side_effect=fake_run),
        ):
            result = protocol._local_run(
                "test-obligation",
                source,
                self.store,
                launch_token="test-local-run-token",
            )
        self.assertEqual(result, 0)
        record = obligations.get_record("test-obligation", self.store)
        self.assertEqual(record["local"]["cost"]["actual"]["cpu_seconds"], 20.0)
        self.assertEqual(record["local"]["cost"]["record_path"], str(cost_path))
        self.assertEqual(record["local"]["receipt_verification"]["state"], "verified")
        self.assertTrue(protocol._local_launch_durable(record))
        self.assertEqual(len(validate_commands), 1)
        launched = validate_commands[0]
        boundary = launched.index("--") + 1
        self.assertEqual(
            launched[boundary : boundary + 2],
            [str(self.root / "ci-hub/ci-hub"), "validate-run"],
        )
        self.assertEqual(
            launched[launched.index("--checkout") + 1], str(workspace / "hermit")
        )
        self.assertEqual(
            launched[launched.index("--agent") + 1], "obligation-test-obligation"
        )
        self.assertEqual(launched[launched.index("--target") + 1], SHA)
        self.assertEqual(launched[launched.index("--log") + 1], str(log_path))
        self.assertNotIn("--in-place", launched)
        self.assertEqual(launched[-2:], ["--", "--no-label-pr"])
        self.assertEqual(
            [argument for argument in launched if argument.endswith("/validate.sh")], []
        )
        self.assertEqual(
            [
                argument
                for argument in launched
                if argument.endswith("/scripts/validate.rs")
            ],
            [],
        )


class DriverCapabilityProbeTest(unittest.TestCase):
    """Bracket the capability probe in BOTH directions, against real binaries.

    These deliberately exec a real script instead of mocking ``_run``: the whole
    point of the probe is that it ASKS the driver, so a test that mocks the
    asking would verify the inference it exists to remove.
    """

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.checkout = Path(self.tmp.name)
        (self.checkout / "scripts").mkdir(parents=True)

    def _write_driver(self, usage: str) -> None:
        driver = self.checkout / "scripts/validate.rs"
        driver.write_text(
            "#!/bin/sh\n" f"cat <<'USAGE'\n{usage}\nUSAGE\n"
        )
        driver.chmod(0o755)

    def test_flag_is_passed_when_the_driver_advertises_it(self) -> None:
        # POSITIVE: a probe that always omitted the flag would satisfy the
        # negative case while silently dropping behaviour. This is the guard.
        self._write_driver(
            "usage: validate.rs <profile> [options]\n"
            "  --no-label-pr    Disable the receipt publication and label update."
        )
        accepted, reason = protocol.driver_accepted_flags(self.checkout)
        self.assertEqual(accepted, ["--no-label-pr"])
        self.assertIn("accepts [--no-label-pr]", reason)

    def test_flag_is_omitted_when_the_driver_does_not_advertise_it(self) -> None:
        # NEGATIVE: this is hermit 649cee3a, which predates the flag by eight
        # minutes. Passing it there aborted the run on a usage error before any
        # test executed, leaving the obligation permanently undischargeable.
        self._write_driver(
            "usage: validate.rs <profile> [options]\n"
            "  -k, --keep-going Do not eager-exit on the first failure."
        )
        accepted, reason = protocol.driver_accepted_flags(self.checkout)
        self.assertEqual(accepted, [])
        self.assertIn("not advertised [--no-label-pr]", reason)

    def test_unknown_is_not_support_when_the_driver_cannot_be_asked(self) -> None:
        # No driver at all: omit, and say so. Never assume support.
        accepted, reason = protocol.driver_accepted_flags(self.checkout)
        self.assertEqual(accepted, [])
        self.assertIn("driver absent", reason)

    def test_a_flag_does_not_match_as_a_prefix_of_a_longer_one(self) -> None:
        # A driver advertising only `--label-pr` CONTAINS the text `--label`.
        # Naive containment would call the shorter flag supported and pass it to
        # a driver that rejects it -- the exact usage-error abort this probe
        # exists to prevent. The boundary must be `[\w-]`, not `\b`: `\b` sits
        # between `l` and `-`, so it would not reject this either.
        usage = "  --label-pr    Publish a receipt and label the PR."
        self.assertFalse(protocol._driver_advertises(usage, "--label"))
        self.assertTrue(protocol._driver_advertises(usage, "--label-pr"))
        # And the negation is a genuinely distinct token from the positive.
        negated = "  --no-label-pr    Disable the receipt publication."
        self.assertFalse(protocol._driver_advertises(negated, "--label"))
        self.assertTrue(protocol._driver_advertises(negated, "--no-label-pr"))

    def test_every_optional_flag_is_gated_not_just_the_known_one(self) -> None:
        # The mechanism must generalise: a future flag added to
        # OPTIONAL_DRIVER_FLAGS needs no further change in protocol.py.
        self._write_driver(
            "usage: validate.rs <profile> [options]\n"
            "  --no-label-pr    Disable the receipt publication.\n"
            "  --future-flag    Something added later."
        )
        accepted, _ = protocol.driver_accepted_flags(
            self.checkout, ("--no-label-pr", "--future-flag", "--never-existed")
        )
        self.assertEqual(accepted, ["--no-label-pr", "--future-flag"])


class GithubStateClassificationTest(unittest.TestCase):
    """A run conclusion is not a truth value: cancelled/absent != red."""

    def _state(self, status: str, conclusion: str) -> str:
        return protocol._github_state({"status": status, "conclusion": conclusion})

    def test_only_success_is_green(self) -> None:
        self.assertEqual(self._state("completed", "success"), "green")
        self.assertEqual(self._state("completed", "neutral"), "no_result")
        self.assertEqual(self._state("", "success"), "no_result")

    def test_genuine_failures_are_red(self) -> None:
        for conclusion in ("failure", "timed_out", "error", "startup_failure"):
            self.assertEqual(self._state("completed", conclusion), "red", conclusion)

    def test_cancelled_is_no_result_not_red(self) -> None:
        # Regression (task cancelled-run-classified-as-red): a cancelled run misread
        # as red nearly reverted a healthy main. no_result never triggers
        # remediation (only red/error do), so a locally-green land survives.
        self.assertEqual(self._state("completed", "cancelled"), "no_result")

    def test_absence_and_unknown_are_no_result(self) -> None:
        for conclusion in ("skipped", "stale", "action_required", "", "brand_new"):
            self.assertEqual(
                self._state("completed", conclusion), "no_result", conclusion
            )

    def test_incomplete_run_preserves_producer_state(self) -> None:
        self.assertEqual(self._state("in_progress", ""), "running")
        self.assertEqual(self._state("queued", ""), "pending")

    def test_no_result_github_leg_does_not_block_a_green_local(self) -> None:
        # A missing hosted answer is supplemental NO_RESULT, not an AND gate.
        # The exact-SHA local green is sufficient authority and closes the watch.
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp) / "store.jsonl"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="admin",
                actor="tester",
                obligation_id="ob-green-local-noresult",
                path=store,
            )
            obligations.transition(
                "ob-green-local-noresult",
                "legs",
                {
                    "local": local_green(),
                    "github": {"state": "no_result"},
                },
                store,
            )
            with (
                mock.patch.object(protocol, "github_main_sha", return_value=SHA),
                mock.patch.object(
                    protocol,
                    "_dereference_current_local_receipt",
                    return_value=(True, verified_local_receipt()),
                ),
            ):
                record = protocol.evaluate_obligation(
                    "ob-green-local-noresult", store_path=store
                )
            self.assertNotEqual(record.get("overall_state"), "remediation_required")
            self.assertEqual(record.get("overall_state"), "satisfied")
            events = [json.loads(line) for line in store.read_text().splitlines()]
            event_types = [event["event_type"] for event in events]
            self.assertLess(
                event_types.index("verification-policy-bound"),
                event_types.index("satisfied"),
            )
            self.assertEqual(
                events[event_types.index("verification-policy-bound")][
                    "verification_policy"
                ],
                protocol.verification_policy_for_repo("rrnewton/hermit"),
            )

    def test_persisted_github_green_short_circuits_supplemental_local_hole(
        self,
    ) -> None:
        """A complete hosted answer must not launch an unnecessary local fill."""
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp) / "store.jsonl"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="admin",
                actor="tester",
                obligation_id="ob-green-github-local-noresult",
                path=store,
            )
            obligations.transition(
                "ob-green-github-local-noresult",
                "legs",
                {
                    "local": {
                        "state": "no_result",
                        "source": str(Path(tmp) / "hermit"),
                        "redispatch_count": 0,
                    },
                    "github": {"state": "green"},
                },
                store,
            )
            with (
                mock.patch.object(protocol, "github_runs") as github_runs,
                mock.patch.object(protocol, "_spawn_detached") as spawn,
            ):
                record = protocol.poll_obligation(
                    "ob-green-github-local-noresult", store
                )
            github_runs.assert_not_called()
            spawn.assert_not_called()
            self.assertEqual(record["overall_state"], "satisfied")
            self.assertEqual(record["local"]["state"], "no_result")


class LocalStateClassificationTest(unittest.TestCase):
    """A local validate exit code is not a truth value: OOM/SIGKILL != red."""

    def test_clean_exit_is_green(self) -> None:
        self.assertEqual(protocol._local_state(0), "green")

    def test_clean_nonzero_exit_without_a_test_verdict_is_no_result(self) -> None:
        # DERIVED discriminator (task cancellation_taxonomy_distinguish_self): a
        # bare nonzero exit with no visible failing-test verdict is NOT a red.
        # We could not read a product test failure, so the failure came from the
        # build/harness/sandbox layer (or is unrecognised) -> re-dispatch, never
        # revert. Putting the unknown on the safe side is what stops a growing
        # list of environmental wordings from ever manufacturing a false revert.
        for code in (1, 2, 3, 42):
            self.assertEqual(protocol._local_state(code), "no_result", code)

    def test_nonzero_exit_with_a_test_verdict_is_red(self) -> None:
        # The one thing that DOES make a local leg red: a genuine failing test.
        for code in (1, 101):
            self.assertEqual(
                protocol._local_state(code, "test result: FAILED. 1 failed"),
                "red",
                code,
            )

    def test_environment_kill_is_no_result_not_red(self) -> None:
        # Regression (task obligation-path-must-consume-no-result-taxonomy): an
        # OOM/SIGKILL (137) sub-profile build was misread as red and helped drive
        # an automated revert of a healthy main tip. A killed process never
        # delivered a verdict; it is a hole to re-dispatch.
        for code in (137, 143, -9, -15):
            self.assertEqual(protocol._local_state(code), "no_result", code)


class EnvironmentalLocalClassificationTest(unittest.TestCase):
    """Plant every direction: a HARNESS-caused red is no_result, never a revert.

    Regression (task cancellation_taxonomy_distinguish_self): three environmental
    failures tonight (a sandbox EPERM re-validate, a BpfJailer `.o.d` denial, a
    cold-build link flake with zero tests) each read as a product red and were
    one automated step from reverting a healthy tip.
    """

    def test_sandbox_eperm_with_zero_test_failures_is_no_result(self) -> None:
        # #1576: a re-validate died on a sandbox EPERM, no test ever ran.
        output = "make: *** [foo.o] Error 1\nopenat(...) = -1 EPERM (Operation not permitted)\n"
        state, reason = protocol._classify_local(1, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:sandbox-denied")

    def test_bpfjailer_dep_write_denial_is_no_result(self) -> None:
        # step-21: BpfJailer denied a `.o.d` dependency-file write.
        output = "cc1: fatal error: could not open dr_config.h.o.d: Permission denied\n"
        state, reason = protocol._classify_local(2, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:sandbox-denied")

    def test_cold_build_link_flake_is_no_result(self) -> None:
        # DynamoRIO cold-build link flake, ZERO test failures.
        output = "/usr/bin/ld: cannot find -lfoo\ncollect2: error: ld returned 1 exit status\n"
        state, reason = protocol._classify_local(1, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:cold-build-flake")

    def test_dynamorio_build_summary_is_build_no_result_not_test_failure(self) -> None:
        # Negative half of the mixed-output bracket. Exact 3801a7df shape: the
        # DAG aggregate says ``1 failed``, but the only concrete operation is
        # DynamoRIO configure/install, before a named product test. The old
        # count regex mislabeled this test-failure.
        for operation in (
            "failed to build and install DynamoRIO: exit status: 2",
            "failed to configure DynamoRIO: exit status: 1",
        ):
            with self.subTest(operation=operation):
                output = (
                    "❌ portable CI DAG lane (0 passed, 1 failed, exit 1: "
                    f"[doc.rustdoc] {operation})\n"
                    "❌ Validation summary [full] (4 passed, 1 failed)\n"
                )
                state, reason = protocol._classify_local(1, output)
                self.assertEqual(state, "no_result")
                self.assertEqual(reason, "non-test-failure:build-tool")

    def test_dag_runner_node_aggregate_is_not_a_product_test_verdict(self) -> None:
        # REAL incident, planted verbatim: obligation
        # 20260809-012006-649cee3a8339-dad6e4 at hermit 649cee3a. The run had ZERO
        # test-failure markers of any kind; the red rested ENTIRELY on the literal
        # text "1 failed" inside safe-ci-dag-runner's own closing summary, whose
        # counts are DAG NODES. The failing node was a TIMEOUT -- its cgroup held
        # processes in do_signal_stop for ~210s on a node that finishes in 54-58s
        # unfrozen -- so an infrastructure freeze was rendered as a product red.
        #
        # _has_explicit_product_test_failure already excluded this text "because
        # it may count DAG nodes"; the catch-all was re-admitting it.
        for runner in ("dagrun", "safe-ci-dag-runner"):
            with self.subTest(runner=runner):
                output = (
                    "[e2e.metadata] ✗ FAIL   metadata (233s, TIMEOUT >60s)\n"
                    f"{runner}: FAIL - 8 passed, 1 failed, 0 aborted, "
                    "39 skipped in 234.1s\n"
                )
                state, reason = protocol._classify_local(1, output)
                self.assertEqual(state, "no_result")
                self.assertNotEqual(reason, "test-failure")

    def test_node_aggregate_strip_is_line_scoped_not_a_blanket_disable(self) -> None:
        # A fix that stopped manufacturing reds by never counting again would be
        # as useless as the bug. The aggregate is removed by LINE, so a genuine
        # bare count elsewhere in the same log still reds.
        runner = (
            "safe-ci-dag-runner: FAIL - 8 passed, 1 failed, 0 aborted, "
            "39 skipped in 234.1s\n"
        )
        with self.subTest("bare count alone still reds"):
            state, reason = protocol._classify_local(1, "collected 12 items\n3 failed, 9 passed\n")
            self.assertEqual((state, reason), ("red", "test-failure"))
        with self.subTest("bare count ALONGSIDE the aggregate still reds"):
            state, reason = protocol._classify_local(1, "3 failed, 9 passed\n" + runner)
            self.assertEqual((state, reason), ("red", "test-failure"))
        with self.subTest("libtest verdict alongside the aggregate still reds"):
            output = "test result: FAILED. 2 passed; 1 failed; 0 ignored\n" + runner
            state, reason = protocol._classify_local(1, output)
            self.assertEqual((state, reason), ("red", "test-failure"))

    def test_a_node_that_could_not_run_is_never_a_product_red(self) -> None:
        # The three outcomes, at the classifier: a node can be "failed" because it
        # TIMED OUT, was CANCELLED, or was ABORTED. None of those is a product test
        # verdict, and collapsing them into red is what trains a fleet to distrust
        # real reds.
        for label, node_line in (
            ("timeout", "[e2e.metadata] ✗ FAIL metadata (233s, TIMEOUT >60s)"),
            ("cpu-timeout", "[build.dbt] ✗ FAIL dbt (61s, CPU-TIMEOUT >60s cpu)"),
            ("oom", "[test.big] ✗ FAIL big (12s, OOM-KILLED (hit inner MemoryMax))"),
        ):
            with self.subTest(label):
                output = (
                    f"{node_line}\n"
                    "safe-ci-dag-runner: FAIL - 0 passed, 1 failed, 0 aborted, "
                    "0 skipped in 234.1s\n"
                )
                state, _ = protocol._classify_local(1, output)
                self.assertEqual(state, "no_result", f"{label} must not be a product red")

    def test_explicit_test_verdict_wins_over_unrelated_build_marker(self) -> None:
        # Positive half of the mixed-output bracket. Combined DAG output can
        # carry an unrelated build-node marker and a genuine product-test
        # verdict. The explicit named test + libtest summary is the stronger
        # causal evidence and must remain red; canonical FAILED/3 receipt
        # binding remains the final remediation authority.
        for operation in (
            "failed to build and install DynamoRIO: exit status: 2",
            "failed to configure DynamoRIO: exit status: 1",
        ):
            with self.subTest(operation=operation):
                output = (
                    f"[build.dbi_release] {operation}\n"
                    "test tests::determinism_holds ... FAILED\n"
                    "test result: FAILED. 41 passed; 1 failed; 0 ignored\n"
                    "❌ Validation summary [full] (4 passed, 2 failed)\n"
                )
                state, reason = protocol._classify_local(1, output)
                self.assertEqual(state, "red")
                self.assertEqual(reason, "test-failure")

    def test_build_script_panic_dag_summary_is_no_result_not_red(self) -> None:
        # REAL incident log (obligation 20260804-025419-0f891e43), planted verbatim:
        # a reverie-dbi/build.rs:339 cold-build panic that the DAG runner rendered
        # with "N failed" + "panicked at". Both the "panicked at" marker and the
        # "\bN failed\b" count regex fire, so the OLD classifier called it a red and
        # armed a revert of a healthy tip. A build.rs panic is a BUILD-phase flake,
        # never a test verdict -> no_result, re-dispatch.
        output = (
            "❌ portable CI DAG manifest (0 passed, 1 failed, exit 1: "
            "[build.dbi_release] thread 'main' (3550207) panicked at "
            ".cargo/git/checkouts/reverie-2fc770f7a9c80803/d973a85/"
            "reverie-dbi/build.rs:339:5:; full log: /tmp/hermit-validate.4bKorf.log)\n"
            "❌ Validation summary [full] (3 passed, 2 failed; full log: "
            "/tmp/hermit-validate.4bKorf.log)\n"
        )
        state, reason = protocol._classify_local(1, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:build-script")

    def test_cargo_custom_build_command_failure_is_no_result(self) -> None:
        output = (
            "error: failed to run custom build command for `reverie-dbi v0.1.0`\n"
            "  process didn't exit successfully (exit status: 101)\n"
            "test summary: 0 passed, 1 failed\n"
        )
        state, reason = protocol._classify_local(101, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:build-script")

    def test_genuine_test_panic_outside_build_rs_stays_red(self) -> None:
        # Guard against over-broadening: a panic in PRODUCT source (not build.rs)
        # with a failing test verdict is a real red and MUST still revert.
        output = (
            "thread 'tests::determinism' panicked at src/detcore/sched.rs:88:5:\n"
            "test result: FAILED. 40 passed; 1 failed; 0 ignored\n"
        )
        state, reason = protocol._classify_local(101, output)
        self.assertEqual(state, "red")
        self.assertEqual(reason, "test-failure")

    def test_inner_memorymax_oom_in_test_node_is_no_result_not_red(self) -> None:
        # Sixth env signature (task cancellation_taxonomy_distinguish_self note
        # 2026-08-04, validate SHA 37f8ef3c): an inner-cgroup MemoryMax OOM inside a
        # boxed TEST node SIGKILLs a test process, so the node surfaces a genuine
        # test-failure VERDICT ("N failed"/panic). Build-script recognition alone
        # does NOT catch this (no build.rs panic), so without the OOM signature
        # _has_test_failures would fire -> false red. The OOM string is the tell:
        # this is a cap breach to re-dispatch (fix-forward = raise the cap), never a
        # tip to revert. Runner strings planted verbatim (model.rs / scheduler.rs).
        output = (
            "[test.hermit_integration] ▲ MEMORY CAP HIT: OOM-killed at its inner "
            "cgroup MemoryMax (4.0 GiB); peak 4.0 GiB, 14 oom_kill event(s)\n"
            "❌ portable CI DAG manifest (0 passed, 1 failed, exit 1: "
            "[test.hermit_integration] OOM-KILLED (hit inner MemoryMax; 14 oom_kill "
            "event(s)))\n"
            "thread 'detcore::sched::tests::t' panicked at src/lib.rs:9:1: SIGKILL\n"
            "test result: FAILED. 20 passed; 1 failed; 0 ignored\n"
        )
        state, reason = protocol._classify_local(1, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:inner-memorymax-oom")

    def test_inner_memorymax_oom_that_also_panicked_build_rs_labels_oom(self) -> None:
        # When an OOM reaps a child rustc/cc1plus and reverie-dbi/build.rs then
        # panics, BOTH the OOM string and the build.rs panic are present. The OOM
        # signature is checked FIRST because "raise this node's cap" is the
        # actionable label; the outcome is no_result either way, never a revert.
        output = (
            "[build.dbi_release] ▲ MEMORY CAP HIT: OOM-killed at its inner cgroup "
            "MemoryMax (8.0 GiB); peak 8.0 GiB, 1 oom_kill event(s)\n"
            "thread 'main' panicked at reverie-dbi/build.rs:339:5: rustc killed\n"
            "❌ Validation summary [full] (3 passed, 2 failed)\n"
        )
        state, reason = protocol._classify_local(1, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:inner-memorymax-oom")

    def test_genuine_test_failure_without_oom_stays_red(self) -> None:
        # Symmetric guard: a real failing test verdict with NO OOM string is still a
        # red that must revert -- the OOM signature never swallows a genuine failure.
        output = "test result: FAILED. 40 passed; 1 failed; 0 ignored\n"
        state, reason = protocol._classify_local(101, output)
        self.assertEqual(state, "red")
        self.assertEqual(reason, "test-failure")

    def test_network_proxy_drop_is_no_result(self) -> None:
        output = "error: failed to get `serde`\nCaused by: could not resolve host: github.com\n"
        state, _ = protocol._classify_local(101, output)
        self.assertEqual(state, "no_result")

    def test_disk_exhaustion_is_no_result(self) -> None:
        output = "error: No space left on device (os error 28)\n"
        state, _ = protocol._classify_local(1, output)
        self.assertEqual(state, "no_result")

    def test_a_real_test_failure_is_red_even_with_a_sandbox_line(self) -> None:
        # A real test failure is NEVER swallowed: it wins over any infra line.
        output = (
            "some background noise: permission denied on /proc\n"
            "test tests::determinism_holds ... FAILED\n"
            "test result: FAILED. 41 passed; 1 failed; 0 ignored\n"
        )
        state, reason = protocol._classify_local(101, output)
        self.assertEqual(state, "red")
        self.assertEqual(reason, "test-failure")

    def test_pytest_style_failure_count_is_red(self) -> None:
        state, reason = protocol._classify_local(1, "=== 3 failed, 200 passed in 4.2s ===")
        self.assertEqual(state, "red")
        self.assertEqual(reason, "test-failure")

    def test_local_compile_break_is_no_result_not_a_local_revert(self) -> None:
        # A post-land compile break of code that ALREADY compiled+tested before
        # arming is overwhelmingly environmental (cold cache/toolchain), so the
        # LOCAL leg does not revert on it: no failing test verdict -> no_result,
        # re-dispatch. A genuine regression the environment did not cause still
        # reverts via the authoritative GitHub leg, which sees it too.
        output = "error[E0425]: cannot find value `foo` in this scope\nerror: could not compile `hermit`\n"
        state, reason = protocol._classify_local(101, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:unclassified")

    def test_never_before_seen_environmental_wording_is_no_result(self) -> None:
        # THE derivation the growing-list trap demands: an environmental failure
        # whose wording is in NONE of our signature categories must STILL be a
        # no_result, because the discriminator is "no failing test verdict", not
        # "matches a known infra string". A missing signature costs only a vague
        # log label ("unclassified"), never a false red / revert.
        output = "quux-jailer: request refused by frobnicator policy 0x9\nBuild step failed.\n"
        state, reason = protocol._classify_local(1, output)
        self.assertEqual(state, "no_result")
        self.assertEqual(reason, "non-test-failure:unclassified")

    def test_environmental_local_red_never_remediates(self) -> None:
        # End-to-end: an environmental local leg stays no_result through the
        # actuator, so no obligation ever recommends a revert for it.
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp) / "store.jsonl"
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="speculative",
                actor="tester",
                obligation_id="ob-env",
                path=store,
            )
            obligations.transition(
                "ob-env",
                "legs",
                {
                    "local": {
                        "state": "no_result",
                        "exit_code": 1,
                        "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                        "source": None,
                    },
                    "github": {"state": "running"},
                },
                store,
            )
            with mock.patch.object(protocol, "github_main_sha", return_value=SHA):
                record = protocol.evaluate_obligation("ob-env", store_path=store)
            self.assertNotEqual(record.get("overall_state"), "remediation_required")


class LocalRedispatchTest(unittest.TestCase):
    """A no_result / uncorroborated red re-dispatches instead of remediating."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.store = self.root / "obligations.jsonl"
        self.source = self.root / "hermit"
        self.source.mkdir()
        obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=SHA,
            land_mode="speculative",
            actor="tester",
            obligation_id="ob",
            path=self.store,
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _seed(self, patch: dict) -> None:
        obligations.transition("ob", "seed", patch, self.store)

    def _spawn_and_register(self, arguments, _log_path):
        arguments = list(arguments)
        token = arguments[arguments.index("--launch-token") + 1]
        protocol._register_local_runner("ob", token, self.store, pid=4321)
        return 4321

    def test_no_result_local_re_dispatches_and_never_remediates(self) -> None:
        self._seed(
            {
                "local": {
                    "state": "no_result",
                    "exit_code": 137,
                    "source": str(self.source),
                    "redispatch_count": 0,
                    "pid": None,
                },
                "github": {"state": "running"},
            }
        )
        with mock.patch.object(
            protocol, "_spawn_detached", side_effect=self._spawn_and_register
        ) as spawn, mock.patch.object(
            protocol,
            "_redispatch_watch_admission",
            return_value=(True, "owner-watch-admitted-since-release"),
        ), mock.patch.object(protocol, "github_runs", return_value=[]):
            with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
                record = protocol.poll_obligation("ob", self.store)
        spawn.assert_called_once()
        self.assertEqual(record["local"]["state"], "running")
        self.assertEqual(record["local"]["redispatch_count"], 1)
        self.assertNotEqual(record["overall_state"], "remediation_required")

    def test_no_result_reports_bounded_producer_window_and_receipt_polling(self) -> None:
        self._seed(
            {
                "local": {
                    "state": "no_result",
                    "source": str(self.source),
                    "redispatch_count": 0,
                    "pid": None,
                }
            }
        )
        awaiting = obligations.get_record("ob", self.store)
        self.assertEqual(
            protocol._local_receipt_disposition(awaiting)["state"],
            "awaiting_receipt",
        )
        self.assertTrue(
            protocol._local_receipt_disposition(awaiting)[
                "will_consume_later_receipt"
            ]
        )

        self._seed(
            {
                "local": {
                    "state": "no_result",
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                }
            }
        )
        exhausted = obligations.get_record("ob", self.store)
        disposition = protocol._local_receipt_disposition(exhausted)
        self.assertEqual(disposition["state"], "awaiting_external_receipt")
        self.assertTrue(disposition["will_consume_later_receipt"])
        self.assertEqual(disposition["automatic_rechecks_remaining"], 0)

    def test_late_exact_sha_receipt_closes_exhausted_no_result(self) -> None:
        self._seed(
            {
                "local": {
                    "state": "no_result",
                    "source": str(self.source),
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                    "pid": None,
                },
                "github": {"state": "no_result"},
            }
        )
        with mock.patch.object(
            protocol,
            "_dereference_current_local_receipt",
            return_value=(True, verified_local_receipt(SHA)),
        ) as dereference:
            record = protocol.poll_obligation("ob", self.store)
        dereference.assert_called()
        self.assertEqual(record["local"]["state"], "green")
        self.assertEqual(record["overall_state"], "satisfied")
        self.assertEqual(
            record["local"]["receipt_verification"]["report"]["sha"], SHA
        )
        self.assertEqual(
            protocol._local_receipt_disposition(record)["state"], "receipt_observed"
        )

    def test_late_different_sha_receipt_does_not_close_no_result(self) -> None:
        self._seed(
            {
                "local": {
                    "state": "no_result",
                    "source": str(self.source),
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                    "pid": None,
                },
                "github": {"state": "no_result"},
            }
        )
        before = obligations.get_record("ob", self.store)
        with mock.patch.object(
            protocol,
            "_dereference_current_local_receipt",
            return_value=(True, verified_local_receipt(NEXT_SHA)),
        ):
            record = protocol.poll_obligation("ob", self.store)
            event_after_policy_bind = record["event_id"]
            record = protocol.poll_obligation("ob", self.store)
        self.assertEqual(record["event_id"], event_after_policy_bind)
        self.assertEqual(record["local"], before["local"])
        self.assertEqual(record["local"]["state"], "no_result")
        self.assertNotEqual(record["overall_state"], "satisfied")
        self.assertEqual(
            protocol._local_receipt_disposition(record)["state"],
            "awaiting_external_receipt",
        )

    def test_provisional_local_red_re_dispatches_not_reverts(self) -> None:
        self._seed(
            {
                "local": {
                    "state": "red",
                    "exit_code": 1,
                    "source": str(self.source),
                    "redispatch_count": 0,
                    "pid": None,
                },
                "github": {"state": "running"},
            }
        )
        with mock.patch.object(
            protocol, "_spawn_detached", side_effect=self._spawn_and_register
        ) as spawn, mock.patch.object(
            protocol,
            "_redispatch_watch_admission",
            return_value=(True, "owner-watch-admitted-since-release"),
        ), mock.patch.object(protocol, "github_runs", return_value=[]):
            with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
                record = protocol.poll_obligation("ob", self.store)
        spawn.assert_called_once()
        self.assertEqual(record["local"]["state"], "running")
        self.assertNotEqual(record["overall_state"], "remediation_required")

    def test_watch_starved_redispatch_yields_without_spawning_or_spending(self) -> None:
        self._seed(
            {
                "local": {
                    "state": "red",
                    "exit_code": 1,
                    "source": str(self.source),
                    "redispatch_count": 0,
                    "pid": None,
                },
                "github": {"state": "running"},
            }
        )
        record = obligations.get_record("ob", self.store)
        with mock.patch.object(
            protocol,
            "_redispatch_watch_admission",
            return_value=(False, "owner-watch-not-admitted-since-release"),
        ), mock.patch.object(protocol, "_spawn_detached") as spawn, redirect_stderr(
            io.StringIO()
        ) as error:
            result = protocol._maybe_redispatch_local("ob", record, self.store)
        spawn.assert_not_called()
        self.assertEqual(result["local"]["state"], "red")
        self.assertEqual(result["local"]["redispatch_count"], 0)
        self.assertIn("LOCAL RE-DISPATCH YIELD", error.getvalue())

    def test_unknown_watch_admission_also_yields(self) -> None:
        self._seed(
            {
                "local": {
                    "state": "no_result",
                    "exit_code": 137,
                    "source": str(self.source),
                    "redispatch_count": 0,
                    "pid": None,
                },
                "github": {"state": "running"},
            }
        )
        record = obligations.get_record("ob", self.store)
        with mock.patch.object(
            protocol,
            "_redispatch_watch_admission",
            return_value=(False, "owner-watch-admission-unknown"),
        ), mock.patch.object(protocol, "_spawn_detached") as spawn:
            result = protocol._maybe_redispatch_local("ob", record, self.store)
        spawn.assert_not_called()
        self.assertEqual(result["local"]["state"], "no_result")
        self.assertEqual(result["local"]["redispatch_count"], 0)

    def test_public_watch_predicate_preserves_three_state_contract(self) -> None:
        release = self.root / ".validate-lock.released"
        verifier = self.root / "owner-watch.py"

        admitted, detail = protocol._redispatch_watch_admission(
            release_path=release, verifier=verifier
        )
        self.assertTrue(admitted)
        self.assertEqual(detail, "bootstrap-no-release-yet")

        release.write_text(
            "schema_version=1\nreleased_at=123\nreleased_by=background\n",
            encoding="utf-8",
        )
        cases = (
            (0, "ADMITTED", True, "owner-watch-admitted-since-release"),
            (1, "NOT_ADMITTED", False, "owner-watch-not-admitted-since-release"),
            (2, "UNKNOWN", False, "owner-watch-admission-unknown"),
        )
        for rc, state, expected, reason in cases:
            with self.subTest(state=state), mock.patch.object(
                protocol,
                "_run",
                return_value=subprocess.CompletedProcess(
                    [str(verifier)], rc, json.dumps({"state": state}), ""
                ),
            ) as run:
                admitted, detail = protocol._redispatch_watch_admission(
                    release_path=release, verifier=verifier
                )
            self.assertEqual(admitted, expected)
            self.assertEqual(detail, reason)
            run.assert_called_once_with(
                (str(verifier), "--admission-status-since", "123"),
                check=False,
                timeout=5,
            )

    def test_spent_budget_red_does_not_re_dispatch(self) -> None:
        # A budget-spent local red no longer re-dispatches (spawn not called). With
        # the hosted leg having given no answer (no_result), the ledger red is
        # authoritative but still cannot auto-revert: it waits for exact owner auth.
        self._seed(
            {
                "local": {
                    "state": "red",
                    "exit_code": 1,
                    "source": str(self.source),
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                    "pid": None,
                },
                "github": {"state": "no_result"},
            }
        )
        with mock.patch.object(
            protocol, "_spawn_detached", return_value=4321
        ) as spawn, mock.patch.object(protocol, "github_runs", return_value=[]):
            with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
                record = protocol.poll_obligation("ob", self.store)
        spawn.assert_not_called()
        self.assertEqual(record["overall_state"], "remediation_required")
        self.assertEqual(record["recommendation"]["action"], "fix-forward")
        self.assertEqual(record["remediation"]["state"], "triggered")

    def test_spent_budget_red_with_github_running_waits(self) -> None:
        # SEQUENCING guard end-to-end through poll: budget-spent local red while the
        # GitHub polling resolves this fixture to no_result; the ledger verdict is
        # then loud but remains owner-gated and never dispatches a revert.
        self._seed(
            {
                "local": {
                    "state": "red",
                    "exit_code": 1,
                    "source": str(self.source),
                    "redispatch_count": protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT,
                    "pid": None,
                },
                "github": {"state": "running"},
            }
        )
        with mock.patch.object(
            protocol, "_spawn_detached", return_value=4321
        ) as spawn, mock.patch.object(protocol, "github_runs", return_value=[]):
            with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
                record = protocol.poll_obligation("ob", self.store)
        spawn.assert_not_called()
        self.assertEqual(record["overall_state"], "remediation_required")
        self.assertEqual(record["recommendation"]["action"], "fix-forward")
        self.assertEqual(record["remediation"]["state"], "triggered")


if __name__ == "__main__":
    unittest.main()


# ---------------------------------------------------------------------------
# A gate must not page on SUCCESS. `watch-obligations --gate` returned rc 1 for
# ANY unresolved obligation, so every merge produced "speculative land requires
# immediate attention" while the body said local=running, github=no_result,
# remediation=none. That is the normal state for the minutes after a land, and a
# gate that cries wolf on success is how a fleet learns to skim past the one
# that matters.
#
# Same treatment as the github-main pending repair: alarm on SUSTAINED-unresolved
# rather than on unresolved. All three cases below are planted, including the one
# a naive staleness bound would miss.
# ---------------------------------------------------------------------------

from datetime import datetime as _dt, timedelta as _td, timezone as _tz

NOW = _dt(2026, 8, 8, 16, 0, 0, tzinfo=_tz.utc)


def _obligation(age_secs, state="open", opened=True):
    rec = {
        "obligation_id": f"ob-{age_secs}-{state}",
        "overall_state": state,
        "repo": "rrnewton/hermit",
    }
    if opened:
        rec["opened_at"] = (NOW - _td(seconds=age_secs)).strftime("%Y-%m-%dT%H:%M:%SZ")
    return rec


def test_a_fresh_post_merge_obligation_is_NOT_stale():
    """CASE 1 -- must not page. Local validation in flight is a normal window."""
    fresh = [_obligation(120)]
    assert protocol._stale_obligations(fresh, now=NOW) == []


def test_an_obligation_past_the_bound_IS_stale():
    """CASE 2 -- must page. Unresolved long past what it waits on is a hole."""
    old = [_obligation(3600)]
    assert len(protocol._stale_obligations(old, now=NOW)) == 1


def test_a_FAILED_obligation_is_never_suppressed_by_the_bound():
    """CASE 3 -- the one a naive staleness bound would miss.

    A remediation_required obligation must page at ANY age, including 10 seconds
    old. The bound gates only the `unresolved` branch; `remediation` is computed
    separately and returns rc 2 regardless. Assert the two are not conflated: a
    brand-new FAILED obligation is NOT filtered out by staleness.
    """
    failed_fresh = _obligation(10, state="remediation_required")
    # It is not stale by age...
    assert protocol._stale_obligations([failed_fresh], now=NOW) == []
    # ...but it IS a remediation record, which is what drives rc 2.
    assert failed_fresh["overall_state"] == "remediation_required"


def test_an_unreadable_opened_at_is_treated_as_stale():
    """Fail-CLOSED here, deliberately, and opposite to the refire futility probe.

    This bound SUPPRESSES a page, so anything it cannot age must not receive the
    suppression. (The refire probe withholds an ACTION and therefore fails open.)
    """
    assert len(protocol._stale_obligations([_obligation(0, opened=False)], now=NOW)) == 1
    bad = _obligation(120)
    bad["opened_at"] = "not-a-timestamp"
    assert len(protocol._stale_obligations([bad], now=NOW)) == 1


def test_the_bound_clears_the_slowest_observed_authority_run():
    """1491s was the slowest successful CI (GitHub-managed portable) on hermit
    main. If someone lowers the bound below measured reality the gate starts
    paging on healthy post-merge windows again, which is the defect."""
    assert protocol.OBLIGATION_STALE_AFTER_SECS > 1491.0


def _report_record(*, state="open", remediation_state="none"):
    return {
        "obligation_id": "decision-obligation",
        "overall_state": state,
        "repo": "rrnewton/hermit",
        "landed_sha": SHA,
        "opened_at": "2026-08-01T00:00:00Z",
        "local": {"state": "no_result"},
        "github": {"state": "no_result"},
        "recommendation": (
            {"action": "fix-forward"} if state == "remediation_required" else None
        ),
        "remediation": {
            "state": remediation_state,
            "kind": "fix-forward" if remediation_state != "none" else None,
        },
    }


def test_summary_distinguishes_exhausted_producer_window_from_receipt_polling():
    record = _report_record()
    record["local"]["redispatch_count"] = protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT
    rendered = protocol._summary_line(record)
    assert "local=no_result" in rendered
    assert "local_receipt=awaiting_external_receipt" in rendered


def test_json_status_carries_receipt_window_disposition(tmp_path):
    record = _report_record()
    record["local"]["redispatch_count"] = protocol.DEFAULT_LOCAL_REDISPATCH_LIMIT
    output = io.StringIO()
    with mock.patch.object(
        obligations, "latest_records", return_value={record["obligation_id"]: record}
    ), redirect_stdout(output):
        protocol.print_status(
            tmp_path / "unused-store.jsonl",
            include_closed=True,
            json_output=True,
            gate=False,
            decision_waits_path=tmp_path / "no-decision-waits.json",
        )
    reported = json.loads(output.getvalue())["obligations"][0]
    disposition = reported["local"]["receipt_disposition"]
    assert disposition["state"] == "awaiting_external_receipt"
    assert disposition["will_consume_later_receipt"] is True


def _decision_wait_registry(path):
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "obligations": {
                    "decision-obligation": {
                        "state": "AWAITING-DECISION",
                        "repo": "rrnewton/hermit",
                        "landed_sha": SHA,
                        "blocker": "PR #2059 awaits the owner's review-policy decision",
                    }
                },
            }
        ),
        encoding="utf-8",
    )


def test_named_decision_wait_deescalates_hard_gate_but_remains_visible(tmp_path):
    """A human-blocked stale record is NOTE-worthy, not an executable page."""
    registry = tmp_path / "decision-waits.json"
    _decision_wait_registry(registry)
    record = _report_record()
    output = io.StringIO()
    with mock.patch.object(
        obligations, "latest_records", return_value={record["obligation_id"]: record}
    ), redirect_stdout(output):
        rc = protocol.print_status(
            tmp_path / "unused.jsonl",
            include_closed=False,
            json_output=False,
            gate=True,
            decision_waits_path=registry,
        )
    rendered = output.getvalue()
    assert rc == 0
    assert "state=awaiting-decision" in rendered
    assert "awaiting_decision_count=1" in rendered
    assert "remediation=AWAITING-DECISION" in rendered
    assert "PR #2059 awaits the owner's review-policy decision" in rendered

    output = io.StringIO()
    with mock.patch.object(
        obligations, "latest_records", return_value={record["obligation_id"]: record}
    ), redirect_stdout(output):
        note_rc = protocol.print_status(
            tmp_path / "unused.jsonl",
            include_closed=False,
            json_output=False,
            gate=True,
            awaiting_decision_only=True,
            decision_waits_path=registry,
        )
    assert note_rc == 1
    assert "remediation=AWAITING-DECISION" in output.getvalue()


def test_concrete_remediation_overrides_decision_wait_and_still_hard_escalates(tmp_path):
    """A stale annotation can never hide a genuine fix-forward/revert result."""
    registry = tmp_path / "decision-waits.json"
    _decision_wait_registry(registry)
    record = _report_record(
        state="remediation_required", remediation_state="triggered"
    )
    output = io.StringIO()
    with mock.patch.object(
        obligations, "latest_records", return_value={record["obligation_id"]: record}
    ), redirect_stdout(output):
        rc = protocol.print_status(
            tmp_path / "unused.jsonl",
            include_closed=False,
            json_output=False,
            gate=True,
            decision_waits_path=registry,
        )
    rendered = output.getvalue()
    assert rc == 2
    assert "state=remediation-required" in rendered
    assert "recommendation=fix-forward" in rendered
    assert "remediation=triggered" in rendered
    assert "AWAITING-DECISION" not in rendered


def test_unclassified_stale_obligation_still_hard_escalates(tmp_path):
    """Removing the annotation restores the original fail-closed stale page."""
    empty_registry = tmp_path / "decision-waits.json"
    empty_registry.write_text(
        '{"schema_version": 1, "obligations": {}}', encoding="utf-8"
    )
    record = _report_record()
    with mock.patch.object(
        obligations, "latest_records", return_value={record["obligation_id"]: record}
    ), redirect_stdout(io.StringIO()):
        rc = protocol.print_status(
            tmp_path / "unused.jsonl",
            include_closed=False,
            json_output=False,
            gate=True,
            decision_waits_path=empty_registry,
        )
    assert rc == 1


def test_tick_routes_decision_waits_to_note_without_repolling():
    config = (REMEDIATION.parents[0] / "health" / "tick-hub.yaml").read_text(
        encoding="utf-8"
    )
    decision_block = config.split(
        "- name: speculative_land_obligation_decision_waits", 1
    )[1].split("- name:", 1)[0]
    assert "protocol.py status --gate --awaiting-decision" in decision_block
    assert "kind: note" in decision_block
    assert "watch-obligations" not in decision_block

    action_block = config.split("- name: speculative_land_obligations", 1)[1].split(
        "- name:", 1
    )[0]
    assert "watch-obligations --once --gate" in action_block
    assert "kind: action" in action_block
    assert "skill: hard-warn-coordinator" in action_block
