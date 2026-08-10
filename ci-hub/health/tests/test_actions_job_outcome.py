#!/usr/bin/env python3
"""Brackets for the exact GitHub Actions setup-only authority."""

from __future__ import annotations

import copy
import json
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

HEALTH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HEALTH))

import actions_job_outcome as outcome  # noqa: E402


FIXTURES = Path(__file__).resolve().parent / "fixtures"
POSITIVE_HEAD = "d282a85726a5e0101cad069c2f3d6e2e23b9d6cd"
NEGATIVE_HEAD = "d93d512826c522dff89a27a1aa2d4eda0377796b"
OLD_PIN_JOB = "reverie-pin-is-latest-main"
TRANSITION_PIN_JOB = "reverie-pin-ancestry-and-monotonicity"
NEW_PIN_JOB = "reverie-pin-ancestor-and-monotonic"
LEGACY_WORKFLOW_BLOB = "579f5e7816c7e2844eadfd7018d95ee37c8d8640"
CURRENT_OLD_WORKFLOW_BLOB = "1502659be5e773f9bdfa9d6e6bb7346d77f03ad9"
TRANSITION_WORKFLOW_BLOB = "8aa4c71066d1575d6d3714827be02dd4f7c8b623"
NEW_WORKFLOW_BLOB = "3ce527cb6dc7aede207d3cd07859d922a5241e38"
OLD_GATE_STEP = "Require the latest-Reverie pin gate"
TRANSITION_GATE_STEP = "Require the Reverie-pin ancestry-and-monotonicity gate"
NEW_GATE_STEP = "Require the Reverie-pin ancestor-and-monotonic gate"


def _fixture(job_id: int) -> dict[str, object]:
    return json.loads((FIXTURES / f"actions_job_{job_id}.json").read_text())


def _positive_check(name: str = OLD_PIN_JOB) -> dict[str, object]:
    return {
        "__typename": "CheckRun",
        "completedAt": "2026-08-06T15:46:01Z",
        "conclusion": "FAILURE",
        "detailsUrl": (
            "https://github.com/rrnewton/hermit/actions/runs/31114544049/"
            "job/92660569815"
        ),
        "name": name,
        "startedAt": "2026-08-06T15:40:35Z",
        "status": "COMPLETED",
        "workflowName": "Merge Gate",
    }


def _negative_check() -> dict[str, object]:
    return {
        "__typename": "CheckRun",
        "completedAt": "2026-08-06T15:12:56Z",
        "conclusion": "FAILURE",
        "detailsUrl": (
            "https://github.com/rrnewton/hermit/actions/runs/31110129926/"
            "job/92645431859"
        ),
        "name": "P0 demo gate (demos 1-8)",
        "startedAt": "2026-08-06T15:07:52Z",
        "status": "COMPLETED",
        "workflowName": "P0 Demo Gate (Hermit hot paths)",
    }


def _downstream_check() -> dict[str, object]:
    return {
        "__typename": "CheckRun",
        "completedAt": "2026-08-06T18:20:25Z",
        "conclusion": "FAILURE",
        "detailsUrl": (
            "https://github.com/rrnewton/hermit/actions/runs/31114544049/"
            "job/92670128104"
        ),
        "name": "merge-gate-v4",
        "startedAt": "2026-08-06T18:20:19Z",
        "status": "COMPLETED",
        "workflowName": "Merge Gate",
    }


def _run_payload() -> dict[str, object]:
    api = "https://api.github.com/repos/rrnewton/hermit"
    return {
        "id": 31114544049,
        "name": "Merge Gate",
        "path": ".github/workflows/merge-gate.yml",
        "head_sha": POSITIVE_HEAD,
        "url": f"{api}/actions/runs/31114544049",
        "jobs_url": f"{api}/actions/runs/31114544049/jobs",
        "workflow_url": f"{api}/actions/workflows/319326542",
        "run_attempt": 1,
        "status": "queued",
        "conclusion": None,
    }


def _workflow_contents_payload(
    blob: str = LEGACY_WORKFLOW_BLOB,
) -> dict[str, object]:
    return {
        "type": "file",
        "name": "merge-gate.yml",
        "path": ".github/workflows/merge-gate.yml",
        "sha": blob,
    }


def _workflow_binding(
    blob: str = LEGACY_WORKFLOW_BLOB,
) -> outcome.RegisteredWorkflowBinding:
    binding, error = outcome.verify_registered_workflow_payloads(
        repo="rrnewton/hermit",
        run_id=31114544049,
        head_sha=POSITIVE_HEAD,
        run_payload=_run_payload(),
        contents_payload=_workflow_contents_payload(blob),
    )
    assert not error and binding is not None
    return binding


def _source_payload(name: str = OLD_PIN_JOB) -> dict[str, object]:
    payload = _fixture(92660569815)
    payload["name"] = name
    return payload


def _downstream_payload(step_name: str = OLD_GATE_STEP) -> dict[str, object]:
    payload = _fixture(92670128104)
    steps = payload["steps"]
    assert isinstance(steps, list)
    steps[4]["name"] = step_name
    return payload


class PayloadVerifierTests(unittest.TestCase):
    def verify(
        self,
        check: dict[str, object],
        head: str,
        payload: object,
    ) -> outcome.SetupOnlyVerification:
        return outcome.verify_setup_only_job_payload(
            repo="rrnewton/hermit", check=check, head_sha=head, payload=payload
        )

    def test_live_positive_1665_setup_only_job_is_accepted(self) -> None:
        result = self.verify(_positive_check(), POSITIVE_HEAD, _fixture(92660569815))
        self.assertTrue(result.accepted)
        self.assertEqual(result.kind, "setup-only")
        self.assertEqual(result.run_id, 31114544049)
        self.assertEqual(result.job_id, 92660569815)

    def test_live_positive_1665_downstream_consequence_is_accepted(self) -> None:
        source = self.verify(_positive_check(), POSITIVE_HEAD, _fixture(92660569815))
        result = outcome.verify_prerequisite_no_result_job_payload(
            repo="rrnewton/hermit",
            check=_downstream_check(),
            head_sha=POSITIVE_HEAD,
            payload=_fixture(92670128104),
            source_check=_positive_check(),
            source=source,
            workflow=_workflow_binding(),
        )
        self.assertTrue(result.accepted)
        self.assertEqual(result.kind, "prerequisite-no-result")
        self.assertEqual(result.job_id, 92670128104)
        self.assertEqual(result.source_job_id, 92660569815)

    def test_all_four_reviewed_name_blob_step_contracts_are_accepted(self) -> None:
        contracts = (
            (OLD_PIN_JOB, LEGACY_WORKFLOW_BLOB, OLD_GATE_STEP),
            (OLD_PIN_JOB, CURRENT_OLD_WORKFLOW_BLOB, OLD_GATE_STEP),
            (TRANSITION_PIN_JOB, TRANSITION_WORKFLOW_BLOB, TRANSITION_GATE_STEP),
            (NEW_PIN_JOB, NEW_WORKFLOW_BLOB, NEW_GATE_STEP),
        )
        for name, blob, step_name in contracts:
            with self.subTest(name=name, blob=blob):
                source_check = _positive_check(name)
                source = self.verify(
                    source_check, POSITIVE_HEAD, _source_payload(name)
                )
                result = outcome.verify_prerequisite_no_result_job_payload(
                    repo="rrnewton/hermit",
                    check=_downstream_check(),
                    head_sha=POSITIVE_HEAD,
                    payload=_downstream_payload(step_name),
                    source_check=source_check,
                    source=source,
                    workflow=_workflow_binding(blob),
                )
                self.assertTrue(result.accepted, result.reason)

    def test_crossed_name_blob_or_receipt_contracts_are_refused(self) -> None:
        crossed = (
            (NEW_PIN_JOB, LEGACY_WORKFLOW_BLOB, NEW_GATE_STEP),
            (OLD_PIN_JOB, NEW_WORKFLOW_BLOB, OLD_GATE_STEP),
            (NEW_PIN_JOB, NEW_WORKFLOW_BLOB, OLD_GATE_STEP),
            (NEW_PIN_JOB, TRANSITION_WORKFLOW_BLOB, NEW_GATE_STEP),
            (TRANSITION_PIN_JOB, NEW_WORKFLOW_BLOB, TRANSITION_GATE_STEP),
            (NEW_PIN_JOB, NEW_WORKFLOW_BLOB, TRANSITION_GATE_STEP),
        )
        for name, blob, step_name in crossed:
            with self.subTest(name=name, blob=blob, step=step_name):
                source_check = _positive_check(name)
                source = self.verify(
                    source_check, POSITIVE_HEAD, _source_payload(name)
                )
                result = outcome.verify_prerequisite_no_result_job_payload(
                    repo="rrnewton/hermit",
                    check=_downstream_check(),
                    head_sha=POSITIVE_HEAD,
                    payload=_downstream_payload(step_name),
                    source_check=source_check,
                    source=source,
                    workflow=_workflow_binding(blob),
                )
                self.assertFalse(result.accepted)

    def test_downstream_contract_tampering_never_becomes_no_result(self) -> None:
        source = self.verify(_positive_check(), POSITIVE_HEAD, _fixture(92660569815))
        mutations = {
            "generic-gate-name": ("check-name", "another-gate"),
            "different-workflow": ("check-workflow", "Other"),
            "product-step-ran": ("step-6", "failure"),
            "wrong-failed-step": ("step-5", "success"),
            "later-step": ("append", "success"),
        }
        for label, (field, value) in mutations.items():
            with self.subTest(label=label):
                check = _downstream_check()
                payload = _fixture(92670128104)
                if field == "check-name":
                    check["name"] = value
                    payload["name"] = value
                elif field == "check-workflow":
                    check["workflowName"] = value
                    payload["workflow_name"] = value
                elif field.startswith("step-"):
                    steps = payload["steps"]
                    assert isinstance(steps, list)
                    steps[int(field.split("-")[1]) - 1]["conclusion"] = value
                else:
                    steps = payload["steps"]
                    assert isinstance(steps, list)
                    steps.append(copy.deepcopy(steps[-1]))
                result = outcome.verify_prerequisite_no_result_job_payload(
                    repo="rrnewton/hermit",
                    check=check,
                    head_sha=POSITIVE_HEAD,
                    payload=payload,
                    source_check=_positive_check(),
                    source=source,
                    workflow=_workflow_binding(),
                )
                self.assertFalse(result.accepted)

    def test_downstream_requires_exact_reviewed_workflow_blob(self) -> None:
        for label, target, field, value in (
            ("run-path", "run", "path", ".github/workflows/other.yml"),
            ("run-head", "run", "head_sha", "f" * 40),
            ("run-id", "run", "id", 31114544050),
            ("run-attempt", "run", "run_attempt", 0),
            ("blob", "contents", "sha", "f" * 40),
            ("contents-path", "contents", "path", ".github/workflows/other.yml"),
        ):
            with self.subTest(label=label):
                run_payload = _run_payload()
                contents_payload = _workflow_contents_payload()
                payload = run_payload if target == "run" else contents_payload
                payload[field] = value
                binding, error = outcome.verify_registered_workflow_payloads(
                    repo="rrnewton/hermit",
                    run_id=31114544049,
                    head_sha=POSITIVE_HEAD,
                    run_payload=run_payload,
                    contents_payload=contents_payload,
                )
                self.assertIsNone(binding)
                self.assertTrue(error)

    def test_live_negative_1697_product_job_is_refused(self) -> None:
        result = self.verify(_negative_check(), NEGATIVE_HEAD, _fixture(92645431859))
        self.assertFalse(result.accepted)
        self.assertIn("exactly one step, observed 18", result.reason)

    def test_identity_name_url_head_and_timestamps_are_all_bound(self) -> None:
        payload = _fixture(92660569815)
        mutations = {
            "job-id": ("id", 92660569816),
            "run-id": ("run_id", 31114544050),
            "head": ("head_sha", "f" * 40),
            "api-url": (
                "url",
                "https://api.github.com/repos/other/repo/actions/jobs/1",
            ),
            "html-url": (
                "html_url",
                "https://github.com/other/repo/actions/runs/1/job/1",
            ),
            "run-url": (
                "run_url",
                "https://api.github.com/repos/other/repo/actions/runs/1",
            ),
            "check-url": (
                "check_run_url",
                "https://api.github.com/repos/other/repo/check-runs/1",
            ),
            "name": ("name", "different"),
            "workflow": ("workflow_name", "different"),
            "started": ("started_at", "2026-08-06T15:40:34Z"),
            "completed": ("completed_at", "2026-08-06T15:46:02Z"),
            "status": ("status", "queued"),
            "conclusion": ("conclusion", "cancelled"),
        }
        for label, (field, value) in mutations.items():
            with self.subTest(label=label):
                changed = copy.deepcopy(payload)
                changed[field] = value
                self.assertFalse(
                    self.verify(_positive_check(), POSITIVE_HEAD, changed).accepted
                )

    def test_tampered_rollup_identity_is_refused(self) -> None:
        changes = {
            "repo": "https://github.com/rrnewton/reverie/actions/runs/31114544049/job/92660569815",
            "run": "https://github.com/rrnewton/hermit/actions/runs/1/job/92660569815",
            "job": "https://github.com/rrnewton/hermit/actions/runs/31114544049/job/1",
            "query": "https://github.com/rrnewton/hermit/actions/runs/31114544049/job/92660569815?x=1",
        }
        for label, url in changes.items():
            with self.subTest(label=label):
                check = _positive_check()
                check["detailsUrl"] = url
                self.assertFalse(
                    self.verify(check, POSITIVE_HEAD, _fixture(92660569815)).accepted
                )

    def test_multiple_failure_or_any_later_step_is_refused(self) -> None:
        for conclusion in ("failure", "success", "skipped"):
            with self.subTest(conclusion=conclusion):
                payload = _fixture(92660569815)
                steps = payload["steps"]
                assert isinstance(steps, list)
                steps.append(
                    {
                        "name": "Run product tests",
                        "number": 2,
                        "status": "completed",
                        "conclusion": conclusion,
                        "started_at": "2026-08-06T15:45:36Z",
                        "completed_at": "2026-08-06T15:45:37Z",
                    }
                )
                result = self.verify(_positive_check(), POSITIVE_HEAD, payload)
                self.assertFalse(result.accepted)
                self.assertIn("exactly one step", result.reason)

    def test_paginated_malformed_or_missing_payload_is_refused(self) -> None:
        for payload in (None, [], {}, {"total_count": 1, "jobs": []}):
            with self.subTest(payload=payload):
                self.assertFalse(
                    self.verify(_positive_check(), POSITIVE_HEAD, payload).accepted
                )

    def test_cancelled_stale_and_unknown_rollup_outcomes_never_accept(self) -> None:
        for conclusion in ("CANCELLED", "STALE", "FUTURE"):
            with self.subTest(conclusion=conclusion):
                check = _positive_check()
                check["conclusion"] = conclusion
                self.assertFalse(
                    self.verify(check, POSITIVE_HEAD, _fixture(92660569815)).accepted
                )


class BoundedDereferenceTests(unittest.TestCase):
    def authority(self, **kwargs: object) -> outcome.GitHubActionsJobAuthority:
        return outcome.GitHubActionsJobAuthority(
            "rrnewton/hermit",
            net_wrapper=["with-proxy"],
            deadline=time.monotonic() + 30,
            **kwargs,
        )

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_constructs_exact_job_endpoint_and_caches(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=json.dumps(_fixture(92660569815)),
            stderr="",
        )
        authority = self.authority()
        first = authority("rrnewton/hermit", _positive_check(), POSITIVE_HEAD)
        second = authority("rrnewton/hermit", _positive_check(), POSITIVE_HEAD)
        self.assertTrue(first.accepted)
        self.assertTrue(second.accepted)
        run.assert_called_once()
        command = run.call_args.args[0]
        self.assertEqual(
            command,
            [
                "with-proxy",
                "gh",
                "api",
                "--method",
                "GET",
                "repos/rrnewton/hermit/actions/jobs/92660569815",
            ],
        )

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_batch_proves_source_and_consequence_in_either_order(
        self, run: mock.Mock
    ) -> None:
        payloads = {
            "92660569815": _fixture(92660569815),
            "92670128104": _fixture(92670128104),
        }

        def response(
            command: list[str], **_kwargs: object
        ) -> subprocess.CompletedProcess:
            endpoint = command[-1]
            if "/actions/jobs/" in endpoint:
                job_id = endpoint.rsplit("/", 1)[-1]
                payload = payloads[job_id]
            elif endpoint.endswith("/actions/runs/31114544049"):
                payload = _run_payload()
            elif "/contents/.github/workflows/merge-gate.yml?ref=" in endpoint:
                payload = _workflow_contents_payload()
            else:
                raise AssertionError(f"unexpected endpoint: {endpoint}")
            return subprocess.CompletedProcess(
                args=command,
                returncode=0,
                stdout=json.dumps(payload),
                stderr="",
            )

        run.side_effect = response
        for checks in (
            (_positive_check(), _downstream_check()),
            (_downstream_check(), _positive_check()),
        ):
            with self.subTest(order=[check["name"] for check in checks]):
                authority = self.authority()
                results = authority.verify_failures(
                    "rrnewton/hermit", checks, POSITIVE_HEAD
                )
                by_kind = {result.kind: result for result in results}
                self.assertEqual(set(by_kind), {"setup-only", "prerequisite-no-result"})
                self.assertEqual(
                    by_kind["prerequisite-no-result"].source_job_id,
                    92660569815,
                )

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_batch_recognizes_new_name_contract_without_weakening_source(
        self, run: mock.Mock
    ) -> None:
        source_check = _positive_check(NEW_PIN_JOB)
        payloads = {
            "92660569815": _source_payload(NEW_PIN_JOB),
            "92670128104": _downstream_payload(NEW_GATE_STEP),
        }

        def response(
            command: list[str], **_kwargs: object
        ) -> subprocess.CompletedProcess:
            endpoint = command[-1]
            if "/actions/jobs/" in endpoint:
                payload = payloads[endpoint.rsplit("/", 1)[-1]]
            elif endpoint.endswith("/actions/runs/31114544049"):
                payload = _run_payload()
            elif "/contents/.github/workflows/merge-gate.yml?ref=" in endpoint:
                payload = _workflow_contents_payload(NEW_WORKFLOW_BLOB)
            else:
                raise AssertionError(f"unexpected endpoint: {endpoint}")
            return subprocess.CompletedProcess(
                args=command,
                returncode=0,
                stdout=json.dumps(payload),
                stderr="",
            )

        run.side_effect = response
        results = self.authority().verify_failures(
            "rrnewton/hermit",
            (source_check, _downstream_check()),
            POSITIVE_HEAD,
        )
        self.assertEqual(
            {result.kind for result in results},
            {"setup-only", "prerequisite-no-result"},
        )

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_downstream_without_dereferenced_source_stays_failed(
        self, run: mock.Mock
    ) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=json.dumps(_fixture(92670128104)),
            stderr="",
        )
        (result,) = self.authority().verify_failures(
            "rrnewton/hermit", (_downstream_check(),), POSITIVE_HEAD
        )
        self.assertFalse(result.accepted)
        self.assertIn("exactly one", result.reason)

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_unreviewed_workflow_blob_keeps_downstream_red(
        self, run: mock.Mock
    ) -> None:
        def response(
            command: list[str], **_kwargs: object
        ) -> subprocess.CompletedProcess:
            endpoint = command[-1]
            if endpoint.endswith("/actions/jobs/92660569815"):
                payload = _fixture(92660569815)
            elif endpoint.endswith("/actions/jobs/92670128104"):
                payload = _fixture(92670128104)
            elif endpoint.endswith("/actions/runs/31114544049"):
                payload = _run_payload()
            elif "/contents/.github/workflows/merge-gate.yml?ref=" in endpoint:
                payload = _workflow_contents_payload()
                payload["sha"] = "f" * 40
            else:
                raise AssertionError(f"unexpected endpoint: {endpoint}")
            return subprocess.CompletedProcess(
                args=command,
                returncode=0,
                stdout=json.dumps(payload),
                stderr="",
            )

        run.side_effect = response
        source, downstream = self.authority().verify_failures(
            "rrnewton/hermit",
            (_positive_check(), _downstream_check()),
            POSITIVE_HEAD,
        )
        self.assertTrue(source.accepted)
        self.assertFalse(downstream.accepted)
        self.assertIn("workflow contents sha mismatch", downstream.reason)

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_genuine_product_failure_stays_failed_in_batch(
        self, run: mock.Mock
    ) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=json.dumps(_fixture(92645431859)),
            stderr="",
        )
        (result,) = self.authority().verify_failures(
            "rrnewton/hermit", (_negative_check(),), NEGATIVE_HEAD
        )
        self.assertFalse(result.accepted)
        self.assertIn("exactly one step, observed 18", result.reason)

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_well_shaped_nonexistent_job_retains_failure(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="", stderr="HTTP 404: Not Found"
        )
        result = self.authority()("rrnewton/hermit", _positive_check(), POSITIVE_HEAD)
        self.assertFalse(result.accepted)
        self.assertIn("404", result.reason)

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_verifier_process_error_retains_failure(self, run: mock.Mock) -> None:
        run.side_effect = OSError("cannot execute")
        result = self.authority()("rrnewton/hermit", _positive_check(), POSITIVE_HEAD)
        self.assertFalse(result.accepted)
        self.assertIn("could not start", result.reason)

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_verifier_timeout_retains_failure(self, run: mock.Mock) -> None:
        run.side_effect = subprocess.TimeoutExpired(cmd="gh api", timeout=1)
        result = self.authority()("rrnewton/hermit", _positive_check(), POSITIVE_HEAD)
        self.assertFalse(result.accepted)
        self.assertIn("API exceeded", result.reason)

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_dereference_budget_exhaustion_does_not_call_api(
        self, run: mock.Mock
    ) -> None:
        result = self.authority(max_dereferences=0)(
            "rrnewton/hermit", _positive_check(), POSITIVE_HEAD
        )
        self.assertFalse(result.accepted)
        self.assertIn("budget exhausted", result.reason)
        run.assert_not_called()

    @mock.patch("actions_job_outcome.subprocess.run")
    def test_deadline_exhaustion_does_not_call_api(self, run: mock.Mock) -> None:
        authority = outcome.GitHubActionsJobAuthority(
            "rrnewton/hermit",
            net_wrapper=[],
            deadline=time.monotonic() - 1,
        )
        result = authority("rrnewton/hermit", _positive_check(), POSITIVE_HEAD)
        self.assertFalse(result.accepted)
        self.assertIn("deadline exhausted", result.reason)
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
