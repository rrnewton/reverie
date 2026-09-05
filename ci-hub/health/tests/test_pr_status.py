#!/usr/bin/env python3
"""Tests for the dev-hermit open-PR health tool (gh engine + planner adapter)."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import pr_status


def _rollup(*states: tuple[str, str]) -> list[dict]:
    """Build a statusCheckRollup list; each (status, conclusion) is a CheckRun."""
    return [{"status": s, "conclusion": c} for s, c in states]


def _pr(
    number: int,
    rollup,
    *,
    mergeable="MERGEABLE",
    merge_state="CLEAN",
    draft=False,
    labels=(),
    head="a" * 40,
    comments=(),
):
    review_comments = [
        {
            "id": f"IC_fixture_{number}_{position}",
            "author": {"login": "fixture-reviewer"},
            "url": (
                f"https://github.com/rrnewton/hermit/pull/{number}"
                f"#issuecomment-{number * 1000 + position}"
            ),
            "body": body,
            "createdAt": f"2026-09-04T00:00:{position:02d}Z",
            "includesCreatedEdit": False,
        }
        for position, body in enumerate(comments, 1)
    ]
    return {
        "number": number,
        "title": f"pr {number}",
        "isDraft": draft,
        "mergeable": mergeable,
        "mergeStateStatus": merge_state,
        "labels": [{"name": label} for label in labels],
        "comments": review_comments,
        "statusCheckRollup": rollup,
        "headRefOid": head,
    }


class GhEngineClassificationTests(unittest.TestCase):
    def test_green_red_pending_counts(self) -> None:
        raw = [
            _pr(1, _rollup(("COMPLETED", "SUCCESS"))),
            _pr(2, _rollup(("COMPLETED", "FAILURE")), mergeable="MERGEABLE",
                merge_state="BLOCKED"),
            _pr(3, _rollup(("IN_PROGRESS", ""))),
            _pr(4, []),  # no checks yet => pending
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.open, 4)
        self.assertEqual(status.green, 1)
        self.assertEqual(status.red, 1)
        self.assertEqual(status.pending, 2)
        self.assertEqual(status.real_reds, 1)

    def test_all_open_population_is_not_collapsed_to_ready_non_draft(self) -> None:
        raw = [
            _pr(1, _rollup(("COMPLETED", "SUCCESS"))),
            _pr(2, [], draft=True),
            _pr(3, [], draft=True),
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.all_open, 3)
        self.assertEqual(status.drafts, 2)
        self.assertIsNone(status.population_age_secs)
        self.assertEqual(status.open, 1)

    def test_cancelled_check_is_pending_not_red(self) -> None:
        # Regression (task cancelled-run-classified-as-red): a cancelled check is a
        # hole to re-run, not a product failure. It must reduce to pending.
        self.assertEqual(
            pr_status._rollup_ci_state(_rollup(("COMPLETED", "CANCELLED"))),
            "pending",
        )
        self.assertEqual(
            pr_status._rollup_ci_state(_rollup(("COMPLETED", "ACTION_REQUIRED"))),
            "pending",
        )

    def test_no_result_checks_block_green_without_becoming_red(self) -> None:
        for conclusion in ("CANCELLED", "SKIPPED", "NEUTRAL", "STALE", "FUTURE"):
            with self.subTest(conclusion=conclusion):
                self.assertEqual(
                    pr_status._rollup_ci_state(_rollup(("COMPLETED", conclusion))),
                    "pending",
                )

    def test_real_failure_still_red(self) -> None:
        self.assertEqual(
            pr_status._rollup_ci_state(_rollup(("COMPLETED", "FAILURE"))), "red"
        )

    def test_real_red_failing_only_merge_gate_is_gate_not_product(self) -> None:
        # A mergeable PR whose ONLY failing check is the receipt meta-check is a
        # real-red, but a landing-gate red — not product breakage.
        raw = [
            _pr(
                10,
                [
                    {"name": "merge-gate-v2", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                    {"name": "Regular tests (GitHub-hosted)", "status": "COMPLETED",
                     "conclusion": "SUCCESS"},
                ],
            ),
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.real_reds, 1)
        self.assertEqual(status.gate_reds, 1)
        self.assertEqual(status.product_reds, 0)
        self.assertEqual(status.prs[0]["real_red_kind"], "gate")

    def test_real_red_failing_a_product_test_is_product(self) -> None:
        raw = [
            _pr(
                11,
                [
                    {"name": "merge-gate", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                    {"name": "Regular tests (GitHub-hosted)", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                ],
            ),
        ]
        status = pr_status._classify_gh_prs("rrnewton/reverie", raw)
        self.assertEqual(status.real_reds, 1)
        self.assertEqual(status.product_reds, 1)
        self.assertEqual(status.gate_reds, 0)
        self.assertEqual(status.prs[0]["real_red_kind"], "product")

    def test_pin_freshness_red_with_merge_gate_is_gate_not_product(self) -> None:
        """THE #1711 FIXTURE, measured 2026-08-07.

        Head 2d4866a0 failed exactly `reverie-pin-is-latest-main` +
        `merge-gate-v4` and was reported as real_reds=1 (product=1). It was a
        shared pin drift: the PR pinned reverie dd3c178e, IDENTICAL to hermit
        main, while reverie main had moved to 0ae0c01b. One unrecognised check
        name defeated the all-gate test and manufactured a phantom product break
        that would have sent an agent to debug the PR's code.
        """
        raw = [
            _pr(
                1711,
                [
                    {"name": "reverie-pin-is-latest-main", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                    {"name": "merge-gate-v4", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                ],
            ),
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.real_reds, 1, "still a real red -- it blocks landing")
        self.assertEqual(status.gate_reds, 1)
        self.assertEqual(status.product_reds, 0, "pin drift is not a product break")
        self.assertEqual(status.prs[0]["real_red_kind"], "gate")

    def test_pin_freshness_red_alone_is_gate(self) -> None:
        for name in (
            "reverie-pin-is-latest-main",
            "reverie-pin-ancestry-and-monotonicity",
            "reverie-pin-ancestor-and-monotonic",
            "liteinst2-pin-is-latest-main",
            "Pin Is Latest Main",
        ):
            with self.subTest(check=name):
                raw = [_pr(1712, [{"name": name, "status": "COMPLETED",
                                   "conclusion": "FAILURE"}])]
                status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
                self.assertEqual(status.gate_reds, 1)
                self.assertEqual(status.product_reds, 0)

    def test_pin_gate_substrings_do_not_swallow_product_failures(self) -> None:
        for name in (
            "prefix-reverie-pin-ancestry-and-monotonicity",
            "reverie-pin-ancestry-and-monotonicity-summary",
            "prefix-reverie-pin-ancestor-and-monotonic",
            "reverie-pin-ancestor-and-monotonic-summary",
            "failure summary: reverie-pin-is-latest-main",
            "liteinst2-pin-is-latest-main diagnostic",
            "Pin Is Latest Main summary",
        ):
            with self.subTest(check=name):
                raw = [_pr(1712, [{"name": name, "status": "COMPLETED",
                                   "conclusion": "FAILURE"}])]
                status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
                self.assertEqual(status.gate_reds, 0)
                self.assertEqual(status.product_reds, 1)

    def test_pin_gate_does_not_swallow_a_real_product_failure(self) -> None:
        """The fix must not become a way to hide breakage: a pin red ALONGSIDE a
        genuine product test failure is still product."""
        raw = [
            _pr(
                1713,
                [
                    {"name": "reverie-pin-is-latest-main", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                    {"name": "Regular tests (GitHub-hosted)", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                ],
            ),
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.product_reds, 1)
        self.assertEqual(status.gate_reds, 0)
        self.assertEqual(status.prs[0]["real_red_kind"], "product")

    def test_absence_of_evidence_is_never_product(self) -> None:
        """A red whose rollup names NO failing check, with no exact-head ledger
        failure, cannot be product: nothing identifies a break, so calling it
        product would send an agent to debug code on no evidence. It stays a
        real red so the split never hides it."""
        raw = [_pr(1714, [{"name": "", "status": "COMPLETED",
                           "conclusion": "FAILURE"}])]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.real_reds, 1, "must not be hidden")
        self.assertEqual(status.product_reds, 0, "no evidence => never product")
        self.assertEqual(status.gate_reds, 1)

    def test_core_review_protocol_only_red_is_gate(self) -> None:
        raw = [
            _pr(
                12,
                [
                    {"name": "core-review-protocol", "status": "COMPLETED",
                     "conclusion": "FAILURE"},
                ],
            ),
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.gate_reds, 1)
        self.assertEqual(status.product_reds, 0)

    def test_latest_same_head_duplicate_controls_health(self) -> None:
        sha = "a" * 40
        older = {
            "name": "merge-gate",
            "headSha": sha,
            "status": "COMPLETED",
            "conclusion": "FAILURE",
            "startedAt": "2026-08-04T15:12:05Z",
            "detailsUrl": "https://github.com/o/r/actions/runs/10/job/1",
        }
        newer = {
            "name": "merge-gate",
            "headSha": sha,
            "status": "COMPLETED",
            "conclusion": "SUCCESS",
            "startedAt": "2026-08-04T15:24:36Z",
            "detailsUrl": "https://github.com/o/r/actions/runs/11/job/2",
        }
        for rollup in ([older, newer], [newer, older]):
            with self.subTest(order=[item["conclusion"] for item in rollup]):
                self.assertEqual(
                    pr_status._rollup_ci_state(rollup, head_sha=sha), "green"
                )

    def test_drafts_excluded(self) -> None:
        raw = [_pr(1, _rollup(("COMPLETED", "SUCCESS")), draft=True)]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.open, 0)

    def test_conflicting_red_is_stale_base_not_real(self) -> None:
        raw = [_pr(1, _rollup(("COMPLETED", "FAILURE")),
                   mergeable="CONFLICTING", merge_state="DIRTY")]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.red, 1)
        self.assertEqual(status.real_reds, 0)
        self.assertEqual(status.prs[0]["red_class"], "stale-base")

    def test_unknown_mergeability_red_is_undetermined_not_real(self) -> None:
        # The lazy-mergeability flap: a cold query returns UNKNOWN; such reds must
        # NOT inflate real_reds or trip the outage alarm.
        raw = [
            _pr(i, _rollup(("COMPLETED", "FAILURE")),
                mergeable="UNKNOWN", merge_state="UNKNOWN")
            for i in range(10)
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.red, 10)
        self.assertEqual(status.real_reds, 0)
        self.assertEqual(status.undetermined_reds, 10)
        self.assertFalse(status.outage_suspected)
        self.assertFalse(status.unhealthy)

    def test_outage_only_from_known_real_reds(self) -> None:
        raw = [
            _pr(i, _rollup(("COMPLETED", "FAILURE")),
                mergeable="MERGEABLE", merge_state="BLOCKED")
            for i in range(5)
        ]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.real_reds, 5)
        self.assertTrue(status.outage_suspected)
        self.assertTrue(status.unhealthy)


class SetupOnlyRollupClassificationTests(unittest.TestCase):
    HEAD = "d282a85726a5e0101cad069c2f3d6e2e23b9d6cd"

    @staticmethod
    def failed_check(
        *,
        name: str = "reverie-pin-is-latest-main",
        run: int = 31114544049,
        job: int = 92660569815,
    ) -> dict[str, object]:
        return {
            "__typename": "CheckRun",
            "completedAt": "2026-08-06T15:46:01Z",
            "conclusion": "FAILURE",
            "detailsUrl": (
                f"https://github.com/rrnewton/hermit/actions/runs/{run}/job/{job}"
            ),
            "name": name,
            "startedAt": "2026-08-06T15:40:35Z",
            "status": "COMPLETED",
            "workflowName": "Merge Gate",
        }

    @staticmethod
    def accepted(
        _repo: str, _check: dict[str, object], _head: str
    ) -> pr_status.SetupOnlyVerification:
        return pr_status.SetupOnlyVerification(
            True, "setup only", run_id=31114544049, job_id=92660569815
        )

    @staticmethod
    def refused(
        _repo: str, _check: dict[str, object], _head: str
    ) -> pr_status.SetupOnlyVerification:
        return pr_status.SetupOnlyVerification(False, "identity mismatch")

    def test_1665_setup_only_failure_becomes_visible_no_result(self) -> None:
        rollup = [
            self.failed_check(),
            {
                "__typename": "CheckRun",
                "name": "merge-gate-v4",
                "status": "QUEUED",
                "conclusion": "",
                "detailsUrl": (
                    "https://github.com/rrnewton/hermit/actions/runs/31114544049/"
                    "job/92670128104"
                ),
            },
        ]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [_pr(1665, rollup, head=self.HEAD)],
            setup_only_verifier=self.accepted,
        )
        self.assertEqual(status.pending, 1)
        self.assertEqual(status.red, 0)
        self.assertEqual(status.real_reds, 0)
        self.assertEqual(status.setup_only_no_result_checks, 1)
        self.assertEqual(
            status.prs[0]["setup_only_no_result_checks"],
            ("reverie-pin-is-latest-main",),
        )
        self.assertEqual(status.prs[0]["failing_checks"], ())

    def test_setup_only_failure_blocks_green_even_when_everything_else_passes(
        self,
    ) -> None:
        result = pr_status._classify_rollup(
            "rrnewton/hermit",
            [
                self.failed_check(),
                {
                    "__typename": "CheckRun",
                    "name": "hosted tests",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                },
            ],
            head_sha=self.HEAD,
            setup_only_verifier=self.accepted,
        )
        self.assertEqual(result.state, "pending")
        self.assertEqual(
            result.setup_only_no_result_checks,
            ("reverie-pin-is-latest-main",),
        )

    def test_1697_product_failure_stays_real_red(self) -> None:
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [
                _pr(
                    1697,
                    [
                        self.failed_check(
                            name="P0 demo gate (demos 1-8)",
                            run=31110129926,
                            job=92645431859,
                        )
                    ],
                    head="d93d512826c522dff89a27a1aa2d4eda0377796b",
                )
            ],
            setup_only_verifier=self.refused,
        )
        self.assertEqual(status.pending, 0)
        self.assertEqual(status.red, 1)
        self.assertEqual(status.real_reds, 1)
        self.assertEqual(status.product_reds, 1)
        self.assertEqual(status.setup_only_no_result_checks, 0)
        self.assertEqual(status.prs[0]["failing_checks"], ("P0 demo gate (demos 1-8)",))

    def test_mixed_setup_only_and_product_failure_remains_red(self) -> None:
        def verifier(
            _repo: str, check: dict[str, object], _head: str
        ) -> pr_status.SetupOnlyVerification:
            if check.get("name") == "setup-infra":
                return pr_status.SetupOnlyVerification(
                    True, "setup only", run_id=1, job_id=2
                )
            return pr_status.SetupOnlyVerification(False, "product steps ran")

        result = pr_status._classify_rollup(
            "rrnewton/hermit",
            [
                self.failed_check(name="setup-infra", job=92660569815),
                self.failed_check(name="product", job=92645431859),
            ],
            head_sha=self.HEAD,
            setup_only_verifier=verifier,
        )
        self.assertEqual(result.state, "red")
        self.assertEqual(result.setup_only_no_result_checks, ("setup-infra",))
        self.assertEqual(result.failing_check_names, ("product",))

    def test_batch_authority_preserves_source_and_downstream_no_result(self) -> None:
        class BatchAuthority:
            def __call__(self, *_args: object) -> pr_status.SetupOnlyVerification:
                raise AssertionError("rollup must use the single batch authority")

            def verify_failures(
                self,
                _repo: str,
                checks: tuple[dict[str, object], ...],
                _head: str,
            ) -> tuple[pr_status.SetupOnlyVerification, ...]:
                results = []
                for check in checks:
                    if check.get("name") == "reverie-pin-is-latest-main":
                        results.append(
                            pr_status.SetupOnlyVerification(
                                True,
                                "setup only",
                                31114544049,
                                92660569815,
                                kind="setup-only",
                            )
                        )
                    else:
                        results.append(
                            pr_status.SetupOnlyVerification(
                                True,
                                "prerequisite consequence",
                                31114544049,
                                92670128104,
                                kind="prerequisite-no-result",
                                source_job_id=92660569815,
                            )
                        )
                return tuple(results)

        downstream = self.failed_check(name="merge-gate-v4", job=92670128104)
        downstream["startedAt"] = "2026-08-06T18:20:19Z"
        downstream["completedAt"] = "2026-08-06T18:20:25Z"
        result = pr_status._classify_rollup(
            "rrnewton/hermit",
            [downstream, self.failed_check()],
            head_sha=self.HEAD,
            setup_only_verifier=BatchAuthority(),
        )
        self.assertEqual(result.state, "pending")
        self.assertEqual(
            result.setup_only_no_result_checks,
            ("reverie-pin-is-latest-main",),
        )
        self.assertEqual(result.prerequisite_no_result_checks, ("merge-gate-v4",))
        self.assertEqual(result.failing_check_names, ())
        self.assertIn("source_job=92660569815", result.prerequisite_evidence[0])

    def test_latest_attempt_is_selected_once_for_state_and_names(self) -> None:
        old_product = self.failed_check(name="same", run=10, job=100)
        new_setup = self.failed_check(name="same", run=11, job=101)
        seen: list[str] = []

        def verifier(
            _repo: str, check: dict[str, object], _head: str
        ) -> pr_status.SetupOnlyVerification:
            seen.append(str(check["detailsUrl"]))
            return pr_status.SetupOnlyVerification(True, "setup only", 11, 101)

        for rollup in ([old_product, new_setup], [new_setup, old_product]):
            with self.subTest(order=[c["detailsUrl"] for c in rollup]):
                seen.clear()
                result = pr_status._classify_rollup(
                    "rrnewton/hermit",
                    rollup,
                    head_sha=self.HEAD,
                    setup_only_verifier=verifier,
                )
                self.assertEqual(result.state, "pending")
                self.assertEqual(result.failing_check_names, ())
                self.assertEqual(len(seen), 1)
                self.assertIn("/runs/11/job/101", seen[0])

    def test_verifier_exception_is_fail_closed_and_visible(self) -> None:
        def broken(
            _repo: str, _check: dict[str, object], _head: str
        ) -> pr_status.SetupOnlyVerification:
            raise RuntimeError("boom")

        result = pr_status._classify_rollup(
            "rrnewton/hermit",
            [self.failed_check()],
            head_sha=self.HEAD,
            setup_only_verifier=broken,
        )
        self.assertEqual(result.state, "red")
        self.assertEqual(result.failing_check_names, ("reverie-pin-is-latest-main",))
        self.assertIn("verifier error", result.actions_job_verification_errors[0])

    def test_nonfailure_no_result_never_invokes_setup_verifier(self) -> None:
        for conclusion in ("CANCELLED", "STALE", "FUTURE"):
            with self.subTest(conclusion=conclusion):
                check = self.failed_check()
                check["conclusion"] = conclusion
                verifier = mock.Mock()
                result = pr_status._classify_rollup(
                    "rrnewton/hermit",
                    [check],
                    head_sha=self.HEAD,
                    setup_only_verifier=verifier,
                )
                self.assertEqual(result.state, "pending")
                self.assertEqual(result.setup_only_no_result_checks, ())
                verifier.assert_not_called()


class ReviewProtocolClassificationTests(unittest.TestCase):
    HEAD = "a" * 40

    def classify(
        self,
        *labels: str,
        draft: bool = False,
        comments=(),
        live_records=None,
        permission_error: str | None = None,
    ):
        entry = _pr(12, [], draft=draft, labels=labels, comments=comments)
        if live_records is not None:
            entry["_canonical_review_records"] = live_records
        if permission_error is not None:
            entry["_review_permission_unavailable"] = permission_error
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [entry],
        )
        self.assertEqual(len(status.review_protocol), 1)
        return status.review_protocol[0]

    def live_approval(
        self,
        *,
        actor: str = "maintainer",
        authorized: bool = True,
        permission: str = "write",
    ) -> dict:
        return {
            "id": 12001,
            "html_url": "https://github.com/rrnewton/hermit/pull/12#issuecomment-12001",
            "user": {"login": actor},
            "body": f"APPROVED-AT: claude {self.HEAD}",
            "_review_surface": "issue-comment",
            "_current_review_actor_authorized": authorized,
            "_current_review_actor_login": actor,
            "_current_review_actor_permission": permission,
            "created_at": "2026-09-05T01:00:00Z",
            "updated_at": "2026-09-05T01:00:00Z",
        }

    def test_round_one_and_current_approvals_are_complete(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-codex1",
            "adversarial-review-claude1",
            "passed-review-codex",
            "passed-review-claude",
            comments=(
                f"[adversarial-reviewer agent, CODEX]\nAPPROVED-AT: codex {self.HEAD}",
                f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED-AT: claude {self.HEAD}",
            ),
        )
        self.assertTrue(audit.complete)
        self.assertEqual(audit.review_rounds, "complete")
        self.assertEqual(audit.current_approvals, "complete")

    def test_later_protocol_rounds_are_valid(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-codex4",
            "adversarial-review-claude3",
            "passed-review-codex",
            "passed-review-claude",
            comments=(
                f"[adversarial-reviewer agent, CODEX]\nAPPROVED-AT: codex {self.HEAD}",
                f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED-AT: claude {self.HEAD}",
            ),
        )
        self.assertTrue(audit.complete)
        self.assertEqual(audit.codex_rounds, (4,))
        self.assertEqual(audit.claude_rounds, (3,))

    def test_every_writer_round_is_accepted_by_the_reader(self) -> None:
        for family in pr_status.review_contract.REVIEW_FAMILIES:
            for round_number in pr_status.review_contract.REVIEW_ROUNDS:
                with self.subTest(family=family, round=round_number):
                    audit = self.classify(
                        "post-facto-human-review",
                        pr_status.review_contract.round_label(family, round_number),
                    )
                    self.assertEqual(
                        getattr(audit, f"{family}_rounds"),
                        (round_number,),
                    )

    def test_review_round_codex_is_not_an_activity_label(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "review-round-codex",
            "adversarial-review-claude1",
        )
        self.assertEqual(audit.codex_rounds, ())
        self.assertEqual(audit.review_rounds, "partial")
        self.assertIn(
            "one of adversarial-review-codex1, adversarial-review-codex2, "
            "adversarial-review-codex3, adversarial-review-codex4",
            audit.missing,
        )

    def test_one_sided_review_is_distinct_from_current_approval(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            "passed-review-claude",
            comments=(
                f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED-AT: claude {self.HEAD}",
            ),
        )
        self.assertEqual(audit.review_rounds, "partial")
        self.assertEqual(audit.current_approvals, "partial")
        self.assertIn(
            "one of adversarial-review-codex1, adversarial-review-codex2, "
            "adversarial-review-codex3, adversarial-review-codex4",
            audit.missing,
        )
        self.assertIn("current-approval-codex", audit.missing)

    def test_unbound_cache_label_is_absent_current_authority(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-codex1",
            "passed-review-codex",
        )
        self.assertEqual(audit.current_approvals, "missing")
        self.assertIn("current-approval-codex", audit.missing)
        self.assertIn("passed-review-codex:UNDETERMINED", audit.invalid_labels)

    def test_a_valid_binding_without_a_label_is_current_authority(self) -> None:
        """The label used to be the index, so no label meant the comments were
        never read and a real review reported exactly like no review at all."""
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            comments=(
                f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED-AT: claude {self.HEAD}",
            ),
        )
        self.assertIn("unlabeled-binding:claude", audit.invalid_labels)
        self.assertEqual(audit.current_approvals, "partial")
        self.assertNotIn("current-approval-claude", audit.missing)

    def test_live_shared_authority_requires_current_github_permission(self) -> None:
        authorized = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            live_records=[self.live_approval()],
        )
        outsider = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            "passed-review-claude",
            live_records=[
                self.live_approval(
                    actor="outside-contributor",
                    authorized=False,
                    permission="read",
                )
            ],
        )

        self.assertEqual(authorized.current_approvals, "partial")
        self.assertNotIn("current-approval-claude", authorized.missing)
        self.assertEqual(outsider.current_approvals, "missing")
        self.assertIn("current-approval-claude", outsider.missing)
        self.assertIn(
            "passed-review-claude:UNDETERMINED", outsider.invalid_labels
        )
        self.assertEqual(
            outsider.review_permission_unavailable,
            ("claude:outside-contributor@read",),
        )

    def test_permission_lookup_failure_does_not_fall_back_to_comment_text(self) -> None:
        entry = _pr(
            12,
            [],
            labels=("post-facto-human-review", "adversarial-review-claude1"),
            comments=(f"APPROVED-AT: claude {self.HEAD}",),
        )
        failure = pr_status.approval_authority.ReviewPermissionUnavailable(
            (
                "cannot determine current review authorization for GitHub actor "
                "maintainer: API unavailable"
        )
        )
        with mock.patch.object(
            pr_status.approval_authority,
            "github_review_records",
            side_effect=failure,
        ):
            pr_status._attach_canonical_review_authority(
                "rrnewton/hermit", [entry]
            )
        audit = pr_status._review_protocol_status(entry)
        self.assertIsNotNone(audit)
        assert audit is not None

        self.assertEqual(audit.current_approvals, "missing")
        self.assertIn("current-approval-claude", audit.missing)
        self.assertEqual(len(audit.review_permission_unavailable), 1)
        self.assertIn("API unavailable", audit.review_permission_unavailable[0])

    def test_no_review_is_distinct_from_permission_unavailable(self) -> None:
        empty_entry = _pr(
            12,
            [],
            labels=("post-facto-human-review", "adversarial-review-claude1"),
        )
        empty_entry["_canonical_review_records"] = []
        outsider_entry = _pr(
            13,
            [],
            labels=("post-facto-human-review", "adversarial-review-claude1"),
        )
        outsider_entry["_canonical_review_records"] = [
            self.live_approval(
                actor="outside-contributor",
                authorized=False,
                permission="read",
            )
        ]

        empty_status = pr_status._classify_gh_prs(
            "rrnewton/hermit", [empty_entry]
        )
        unavailable_status = pr_status._classify_gh_prs(
            "rrnewton/hermit", [outsider_entry]
        )
        empty_report = pr_status.render_report(
            [empty_status], warn_threshold=10, engine="gh"
        )
        unavailable_report = pr_status.render_report(
            [unavailable_status], warn_threshold=10, engine="gh"
        )

        self.assertEqual(
            empty_status.review_protocol[0].review_permission_unavailable, ()
        )
        self.assertIn("review_permission_unavailable=0", empty_report)
        self.assertIn("review_permission_unavailable=1", unavailable_report)
        self.assertIn(
            "claude:outside-contributor@read", unavailable_report
        )

    def test_shared_reader_honors_wrapper_command_deadline_and_permission(self) -> None:
        approval = {
            "id": 12001,
            "html_url": (
                "https://github.com/rrnewton/hermit/pull/12"
                "#issuecomment-12001"
            ),
            "user": {"login": "maintainer"},
            "body": f"APPROVED-AT: claude {self.HEAD}",
            "created_at": "2026-09-05T01:00:00Z",
            "updated_at": "2026-09-05T01:00:00Z",
        }
        native_pages = [
            {
                "data": {
                    "repository": {
                        "pullRequest": {
                            "reviews": {
                                "nodes": [],
                                "pageInfo": {
                                    "hasNextPage": False,
                                    "endCursor": None,
                                },
                            }
                        }
                    }
                }
            }
        ]
        commands: list[list[str]] = []
        timeouts: list[float] = []

        def fake_run(command, **kwargs):
            commands.append(command)
            timeouts.append(kwargs["timeout"])
            joined = " ".join(command)
            if "/issues/12/comments" in joined:
                payload = [[approval]]
            elif "/pulls/12/comments" in joined:
                payload = [[]]
            elif " api graphql " in f" {joined} ":
                payload = native_pages
            elif "/collaborators/maintainer/permission" in joined:
                payload = {
                    "user": {"login": "maintainer"},
                    "role_name": "write",
                    "permission": "write",
                }
            else:
                raise AssertionError(f"unexpected command: {command!r}")
            return subprocess.CompletedProcess(
                command, returncode=0, stdout=json.dumps(payload), stderr=""
            )

        with mock.patch.object(
            pr_status.approval_authority.subprocess,
            "run",
            side_effect=fake_run,
        ):
            records = pr_status.approval_authority.github_review_records(
                "rrnewton/hermit",
                12,
                net_wrapper=["custom-wrapper", "--network"],
                gh_cmd="custom-gh",
                deadline=time.monotonic() + 10.0,
            )

        self.assertEqual(len(records), 1)
        self.assertTrue(records[0]["_current_review_actor_authorized"])
        self.assertEqual(len(commands), 4)
        self.assertTrue(
            all(
                command[:3] == ["custom-wrapper", "--network", "custom-gh"]
                for command in commands
            )
        )
        self.assertTrue(all(0 < timeout <= 10.0 for timeout in timeouts))

    def test_ten_millisecond_deadline_refuses_eighty_millisecond_read(self) -> None:
        completed = subprocess.CompletedProcess(
            ["custom-gh"], returncode=0, stdout="[[]]", stderr=""
        )
        with mock.patch.object(
            pr_status.approval_authority.time,
            "monotonic",
            side_effect=[0.0, 0.080],
        ), mock.patch.object(
            pr_status.approval_authority.subprocess,
            "run",
            return_value=completed,
        ) as run:
            with self.assertRaisesRegex(
                pr_status.approval_authority.GitHubDeadlineExceeded,
                "time budget exhausted",
            ):
                pr_status.approval_authority.github_review_records(
                    "rrnewton/hermit",
                    12,
                    net_wrapper=[],
                    gh_cmd="custom-gh",
                    deadline=0.010,
                )

        run.assert_called_once()
        self.assertAlmostEqual(run.call_args.kwargs["timeout"], 0.010)

    def test_actor_permission_read_uses_the_same_deadline(self) -> None:
        approval = {
            "id": 12001,
            "html_url": (
                "https://github.com/rrnewton/hermit/pull/12"
                "#issuecomment-12001"
            ),
            "user": {"login": "maintainer"},
            "body": f"APPROVED-AT: claude {self.HEAD}",
            "created_at": "2026-09-05T01:00:00Z",
            "updated_at": "2026-09-05T01:00:00Z",
        }
        native_pages = [
            {
                "data": {
                    "repository": {
                        "pullRequest": {
                            "reviews": {
                                "nodes": [],
                                "pageInfo": {
                                    "hasNextPage": False,
                                    "endCursor": None,
                                },
                            }
                        }
                    }
                }
            }
        ]

        def fake_run(command, **_kwargs):
            joined = " ".join(command)
            if "/issues/12/comments" in joined:
                payload = [[approval]]
            elif "/pulls/12/comments" in joined:
                payload = [[]]
            elif " api graphql " in f" {joined} ":
                payload = native_pages
            elif "/collaborators/maintainer/permission" in joined:
                payload = {
                    "user": {"login": "maintainer"},
                    "role_name": "write",
                    "permission": "write",
                }
            else:
                raise AssertionError(f"unexpected command: {command!r}")
            return subprocess.CompletedProcess(
                command, returncode=0, stdout=json.dumps(payload), stderr=""
            )

        with mock.patch.object(
            pr_status.approval_authority.time,
            "monotonic",
            side_effect=[0.0, 0.001, 0.002, 0.003, 0.004, 0.005, 0.006, 0.080],
        ), mock.patch.object(
            pr_status.approval_authority.subprocess,
            "run",
            side_effect=fake_run,
        ) as run:
            with self.assertRaisesRegex(
                pr_status.approval_authority.GitHubDeadlineExceeded,
                "time budget exhausted",
            ):
                pr_status.approval_authority.github_review_records(
                    "rrnewton/hermit",
                    12,
                    net_wrapper=[],
                    gh_cmd="custom-gh",
                    deadline=0.010,
                )

        self.assertEqual(run.call_count, 4)
        self.assertIn(
            "/collaborators/maintainer/permission",
            " ".join(run.call_args.args[0]),
        )

    def test_live_fetch_uses_shared_permission_annotated_records(self) -> None:
        entry = _pr(
            12,
            [],
            labels=("post-facto-human-review", "adversarial-review-claude1"),
            comments=(f"APPROVED-AT: claude {self.HEAD}",),
        )
        live = [
            self.live_approval(
                actor="outside-contributor",
                authorized=False,
                permission="read",
            )
        ]
        with mock.patch.object(
            pr_status.approval_authority,
            "github_review_records",
            return_value=live,
        ) as shared_reader, mock.patch(
            "pr_status._fetch_open_prs_gh", return_value=[entry]
        ), mock.patch(
            "pr_status._cached_open_pr_census", return_value=([entry], 0.0)
        ), mock.patch(
            "pr_status.banked_green_commits", return_value=frozenset()
        ), mock.patch(
            "pr_status.banked_failure_tier_commits", return_value={}
        ), mock.patch(
            "pr_status.hosted_authority_states", return_value={}
        ):
            before = time.monotonic()
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit",
                net_wrapper=["custom-wrapper"],
                gh_cmd="custom-gh",
                timeout=30.0,
            )

        shared_reader.assert_called_once()
        args = shared_reader.call_args.args
        kwargs = shared_reader.call_args.kwargs
        self.assertEqual(args, ("rrnewton/hermit", 12))
        self.assertEqual(kwargs["net_wrapper"], ["custom-wrapper"])
        self.assertEqual(kwargs["gh_cmd"], "custom-gh")
        self.assertGreater(kwargs["deadline"], before)
        self.assertTrue(status.available)
        self.assertEqual(len(status.review_protocol), 1)
        audit = status.review_protocol[0]
        self.assertEqual(audit.current_approvals, "missing")
        self.assertEqual(
            audit.review_permission_unavailable,
            ("claude:outside-contributor@read",),
        )

    def test_deadline_exhaustion_between_prs_stops_before_second_read(self) -> None:
        entries = [
            _pr(
                number,
                [],
                labels=("post-facto-human-review",),
            )
            for number in (12, 13)
        ]
        with mock.patch.object(
            pr_status.time,
            "monotonic",
            side_effect=[0.0, 0.005, 0.020],
        ), mock.patch.object(
            pr_status.approval_authority,
            "github_review_records",
            return_value=[],
        ) as reader:
            with self.assertRaisesRegex(
                pr_status.approval_authority.GitHubDeadlineExceeded,
                "time budget exhausted",
            ):
                pr_status._attach_canonical_review_authority(
                    "rrnewton/hermit",
                    entries,
                    net_wrapper=["custom-wrapper"],
                    gh_cmd="custom-gh",
                    deadline=0.010,
                )

        reader.assert_called_once_with(
            "rrnewton/hermit",
            12,
            net_wrapper=["custom-wrapper"],
            gh_cmd="custom-gh",
            deadline=0.010,
        )
        self.assertEqual(entries[0]["_canonical_review_records"], [])
        self.assertNotIn("_canonical_review_records", entries[1])

    def test_review_deadline_exhaustion_is_never_available(self) -> None:
        entry = _pr(
            12,
            [],
            labels=("post-facto-human-review",),
        )
        exhausted = pr_status.approval_authority.GitHubDeadlineExceeded(
            "rrnewton/hermit: time budget exhausted during canonical review authority"
        )
        with mock.patch(
            "pr_status._fetch_open_prs_gh", return_value=[entry]
        ), mock.patch(
            "pr_status._cached_open_pr_census", return_value=([entry], 0.0)
        ), mock.patch(
            "pr_status.banked_green_commits", return_value=frozenset()
        ), mock.patch(
            "pr_status.banked_failure_tier_commits", return_value={}
        ), mock.patch(
            "pr_status._attach_canonical_review_authority",
            side_effect=exhausted,
        ), mock.patch(
            "pr_status.hosted_authority_states"
        ) as hosted:
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[], timeout=30.0
            )

        self.assertFalse(status.available)
        self.assertIn("time budget exhausted", status.reason)
        hosted.assert_not_called()
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("UNAVAILABLE", report)
        self.assertIn("PARTIAL RESULT", report)
        self.assertIn("time budget exhausted", report)


    def test_cache_label_variation_does_not_change_comment_authority(self) -> None:
        comments = (
            f"APPROVED-AT: claude {self.HEAD}",
        )
        without_cache = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            comments=comments,
        )
        with_unrelated_cache = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            "passed-review-codex",
            comments=comments,
        )
        self.assertEqual(without_cache.current_approvals, "partial")
        self.assertEqual(
            without_cache.current_approvals, with_unrelated_cache.current_approvals
        )
        self.assertNotIn("current-approval-claude", with_unrelated_cache.missing)

    def test_an_unrecorded_refusal_remains_visibly_incomplete(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            comments=(
                f"[hermit2, hermit-127, unresolved, devbig014, role=reviewer]\n"
                f"CHANGES-REQUESTED-AT: codex {self.HEAD}",
            ),
        )
        self.assertFalse(audit.complete)
        self.assertEqual(audit.review_rounds, "partial")
        self.assertIn("unlabeled-refusal:codex@aaaaaaaaaaaa", audit.invalid_labels)
        self.assertIn(
            "one of adversarial-review-codex1, adversarial-review-codex2, "
            "adversarial-review-codex3, adversarial-review-codex4",
            audit.missing,
        )

    def test_no_binding_and_no_label_does_not_claim_one(self) -> None:
        """Negative control: without it the assertion above would pass on a build
        that appended the diagnostic unconditionally."""
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
        )
        self.assertNotIn("unlabeled-binding:claude", audit.invalid_labels)
        self.assertNotIn("unlabeled-binding:codex", audit.invalid_labels)

    def test_a_superseded_binding_with_no_label_is_not_surfaced_as_current(self) -> None:
        """Only a CURRENT-head binding is a missing cache over a real review; a
        stale one is just history and must not read as review evidence."""
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-claude1",
            comments=(
                "[adversarial-reviewer agent, CLAUDE]\n"
                "APPROVED-AT: claude 87f62930d3e24ab75b516d01cba106161e0cca2e",
            ),
        )
        self.assertNotIn("unlabeled-binding:claude", audit.invalid_labels)

    def test_invalid_or_bare_round_labels_do_not_fake_review(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-codex",
            "adversarial-review-claude0",
            "adversarial-review-codex5",
            "adversarial-review-claude10",
        )
        self.assertEqual(audit.review_rounds, "missing")
        self.assertEqual(len(audit.invalid_labels), 4)

    def test_suffixed_approval_does_not_mean_current_head_approved(self) -> None:
        audit = self.classify(
            "post-facto-human-review",
            "adversarial-review-codex2",
            "adversarial-review-claude2",
            "passed-review-codex2",
            "passed-review-claude2",
        )
        self.assertEqual(audit.review_rounds, "complete")
        self.assertEqual(audit.current_approvals, "missing")
        self.assertFalse(audit.complete)
        self.assertEqual(len(audit.invalid_labels), 2)

    def test_drafts_stay_out_of_ci_counts_but_in_review_audit(self) -> None:
        audit = self.classify("post-facto-human-review", draft=True)
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [_pr(12, [], draft=True, labels=("post-facto-human-review",))],
        )
        self.assertTrue(audit.draft)
        self.assertEqual(status.open, 0)
        self.assertEqual(len(status.review_protocol), 1)


class MechanismOverlapTests(unittest.TestCase):
    def test_same_mechanism_on_two_open_prs_is_surfaced(self) -> None:
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [
                _pr(1567, [], labels=("mechanism:cancel-in-progress",)),
                _pr(
                    1575,
                    [],
                    draft=True,
                    labels=("mechanism:cancel-in-progress",),
                ),
            ],
        )
        self.assertEqual(len(status.mechanism_overlaps), 1)
        overlap = status.mechanism_overlaps[0]
        self.assertEqual(overlap.mechanism, "mechanism:cancel-in-progress")
        self.assertEqual([pr.pr for pr in overlap.prs], [1567, 1575])
        self.assertTrue(overlap.prs[1].draft)

    def test_distinct_or_singleton_mechanisms_do_not_warn(self) -> None:
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [
                _pr(1, [], labels=("mechanism:CI_DAG_JOBS",)),
                _pr(2, [], labels=("mechanism:locally-validated",)),
                _pr(3, [], labels=("mechanism:",)),
            ],
        )
        self.assertEqual(status.mechanism_overlaps, ())

    def test_report_names_every_pr_in_the_overlap(self) -> None:
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [
                _pr(12, [], labels=("mechanism:locally-validated",)),
                _pr(34, [], labels=("mechanism:locally-validated",)),
            ],
        )
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("Mechanism overlaps: 1", report)
        self.assertIn("mechanism:locally-validated", report)
        self.assertIn("#12", report)
        self.assertIn("#34", report)


class BankedGreenCommitsTests(unittest.TestCase):
    def test_successful_empty_ledger_is_authoritatively_empty(self) -> None:
        """POSITIVE BRACKET: a successful reader may truthfully return EMPTY."""
        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="", stderr=""
        )
        with mock.patch("pr_status.subprocess.run", return_value=completed):
            commits = pr_status.banked_green_commits(
                "rrnewton/hermit", ci_hub_bin=Path(__file__)
            )
        self.assertEqual(commits, frozenset())

    def test_failed_ledger_reader_is_unavailable_not_empty(self) -> None:
        """NEGATIVE BRACKET: reader failure must not become evidence of absence."""
        failures = (
            OSError("cannot execute"),
            subprocess.TimeoutExpired(cmd="qualified-rows", timeout=1.0),
            subprocess.CompletedProcess(
                args=[], returncode=7, stdout="", stderr="ledger exploded"
            ),
        )
        for failure in failures:
            with self.subTest(failure=type(failure).__name__), mock.patch(
                "pr_status.subprocess.run",
                side_effect=failure if isinstance(failure, BaseException) else None,
                return_value=(
                    failure
                    if isinstance(failure, subprocess.CompletedProcess)
                    else None
                ),
            ):
                with self.assertRaisesRegex(
                    pr_status.RepoUnavailable,
                    "local validation ledger unavailable",
                ):
                    pr_status.banked_green_commits(
                        "rrnewton/hermit", ci_hub_bin=Path(__file__), timeout=1.0
                    )

        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "missing-ci-hub"
            with self.assertRaisesRegex(pr_status.RepoUnavailable, "missing reader"):
                pr_status.banked_green_commits(
                    "rrnewton/hermit", ci_hub_bin=missing
                )

    def test_malformed_ledger_answer_is_unavailable_not_empty(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="not-json\n", stderr=""
        )
        with mock.patch("pr_status.subprocess.run", return_value=completed):
            with self.assertRaisesRegex(pr_status.RepoUnavailable, "malformed JSON"):
                pr_status.banked_green_commits(
                    "rrnewton/hermit", ci_hub_bin=Path(__file__)
                )


class GhEngineLoudFailureTests(unittest.TestCase):
    """No output + zero exit must never look like 'no PRs'."""

    @mock.patch("pr_status.subprocess.run")
    def test_empty_stdout_is_unavailable_not_zero_prs(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="", stderr=""
        )
        with self.assertRaisesRegex(pr_status.RepoUnavailable, "EMPTY output"):
            pr_status.fetch_repo_status_gh("rrnewton/hermit", net_wrapper=[])

    @mock.patch("pr_status.subprocess.run")
    def test_bpfjailer_block_is_actionable_unavailable(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="",
            stderr="An action was blocked on this server based on a security policy!",
        )
        with self.assertRaisesRegex(pr_status.RepoUnavailable, "BpfJailer"):
            pr_status.fetch_repo_status_gh("rrnewton/hermit", net_wrapper=[])
        run.assert_called_once()  # a policy block is not retried

    @mock.patch("pr_status.subprocess.run")
    def test_unreachable_network_is_unavailable(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="",
            stderr='dial tcp: connect: network is unreachable',
        )
        with self.assertRaises(pr_status.RepoUnavailable):
            pr_status.fetch_repo_status_gh("rrnewton/hermit", net_wrapper=[])

    @mock.patch("pr_status.subprocess.run")
    def test_non_json_is_unavailable(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="not json", stderr=""
        )
        with self.assertRaises(pr_status.RepoUnavailable):
            pr_status.fetch_repo_status_gh("rrnewton/hermit", net_wrapper=[])

    @mock.patch("pr_status.subprocess.run")
    def test_timeout_is_unavailable_not_hang(self, run: mock.Mock) -> None:
        run.side_effect = subprocess.TimeoutExpired(cmd="gh", timeout=1.0)
        with self.assertRaises(pr_status.RepoUnavailable):
            pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[], timeout=1.0
            )

    @mock.patch("pr_status.time.sleep", lambda _: None)
    @mock.patch("pr_status.subprocess.run")
    def test_truncated_api_response_is_RETRIED_then_succeeds(
        self, run: mock.Mock
    ) -> None:
        """THE REGRESSION. A truncated gh response took out a whole repo.

        gh exits NON-ZERO and prints "unexpected end of JSON input" when the API
        response is cut off mid-stream. That marker was absent from
        _RETRYABLE_MARKERS, so `retryable` was False, the loop broke on attempt
        1, and MAX_FETCH_ATTEMPTS was never reached -- the tool carried a retry
        budget the only failure occurring in practice could not use. Measured on
        2 of 5 consecutive lander polls against rrnewton/hermit.

        Asserts the RETRY happens AND the recovered payload is returned, so a
        regression to break-on-first-attempt fails here rather than silently
        degrading a repo to unavailable.
        """
        payload = '[{"number": 7, "isDraft": false}]'
        run.side_effect = [
            subprocess.CompletedProcess(
                args=[], returncode=1, stdout="",
                stderr="unexpected end of JSON input",
            ),
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=payload, stderr=""
            ),
        ]
        raw = pr_status._fetch_open_prs_gh(
            "rrnewton/hermit", fields=pr_status.GH_FIELDS, net_wrapper=[]
        )
        self.assertEqual(raw, [{"number": 7, "isDraft": False}])
        self.assertEqual(run.call_count, 2, "the truncated response was not retried")

    @mock.patch("pr_status.subprocess.run")
    def test_retry_backoff_is_charged_to_the_total_deadline(
        self, run: mock.Mock
    ) -> None:
        """Retries must not escape the caller's wall-clock budget.

        Each subprocess timeout is only one part of the elapsed time.  The
        retry backoff must consume the same absolute budget; otherwise three
        individually bounded attempts can outlive the pull-request gate that
        owns them.
        """
        clock = [0.0]

        def monotonic() -> float:
            return clock[0]

        def sleep(seconds: float) -> None:
            clock[0] += seconds

        def retryable_failure(*_args: object, **_kwargs: object) -> object:
            clock[0] += 0.5
            return subprocess.CompletedProcess(
                args=[], returncode=1, stdout="", stderr="connection reset"
            )

        run.side_effect = retryable_failure
        with mock.patch("pr_status.time.monotonic", side_effect=monotonic), \
             mock.patch("pr_status.time.sleep", side_effect=sleep):
            with self.assertRaisesRegex(
                pr_status.RepoUnavailable, "time budget exhausted"
            ):
                pr_status._fetch_open_prs_gh(
                    "rrnewton/hermit",
                    fields=pr_status.GH_FIELDS,
                    net_wrapper=[],
                    timeout=2.5,
                )

        self.assertEqual(run.call_count, 2, "a third attempt began after deadline")
        self.assertLessEqual(clock[0], 2.5)

    @mock.patch("pr_status.time.sleep", lambda _: None)
    @mock.patch("pr_status.subprocess.run")
    def test_truncated_api_response_still_fails_closed_when_it_never_recovers(
        self, run: mock.Mock
    ) -> None:
        """Retrying must not become a way to hide a persistent failure."""
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="",
            stderr="unexpected end of JSON input",
        )
        with self.assertRaisesRegex(pr_status.RepoUnavailable, "unexpected end of"):
            pr_status._fetch_open_prs_gh(
            "rrnewton/hermit", fields=pr_status.GH_FIELDS, net_wrapper=[]
        )
        self.assertEqual(run.call_count, pr_status.MAX_FETCH_ATTEMPTS)

    @mock.patch("pr_status.time.sleep", lambda _: None)
    @mock.patch("pr_status.subprocess.run")
    def test_a_non_retryable_error_is_still_not_retried(self, run: mock.Mock) -> None:
        """NEGATIVE SIDE OF THE BRACKET: widening the marker list must not make
        every failure retryable. A genuine auth error is not transient and
        re-running it three times only wastes the budget."""
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="",
            stderr="gh: authentication required",
        )
        with self.assertRaises(pr_status.RepoUnavailable):
            pr_status._fetch_open_prs_gh(
            "rrnewton/hermit", fields=pr_status.GH_FIELDS, net_wrapper=[]
        )
        run.assert_called_once()

    def test_partial_result_line_does_not_assert_a_time_budget(self) -> None:
        """The summary line must not name a cause it does not know.

        It previously said "could not be queried within the time budget" for
        EVERY unavailable repo. The real failure was a truncated response with a
        300s budget on a 5.0s query -- so the message sent the reader to raise a
        timeout that was never binding.
        """
        def _status(repo: str, available: bool) -> pr_status.RepoStatus:
            return pr_status.RepoStatus(
                repo=repo, open=0, green=0, red=0, pending=0, real_reds=0,
                outage_suspected=False, prs=[], available=available,
            )

        statuses = [
            _status("rrnewton/hermit", False),
            _status("rrnewton/reverie", True),
        ]
        report = pr_status.render_report(statuses, warn_threshold=10, engine="gh")
        self.assertIn("PARTIAL RESULT", report)
        self.assertNotIn("time budget", report)
        self.assertIn("rrnewton/hermit", report)

    @mock.patch("pr_status.banked_failure_tier_commits", return_value={})
    @mock.patch("pr_status.banked_green_commits", return_value=frozenset())
    @mock.patch("pr_status.time.sleep")
    @mock.patch("pr_status.subprocess.run")
    def test_transient_stream_error_retries(
        self, run, sleep, _green, _failure
    ) -> None:
        # Isolate the gh-fetch retry path: stub the ledger cross-references so the
        # mocked `subprocess.run` side_effect models ONLY the gh call (error then
        # success), and the test does not read the machine-local ledger.
        run.side_effect = [
            subprocess.CompletedProcess(
                args=[], returncode=1, stdout="",
                stderr="stream error: stream ID 1; CANCEL",
            ),
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout="[]", stderr=""
            ),
        ]
        with mock.patch(
            "pr_status._cached_open_pr_census", return_value=([], 0.0)
        ):
            status = pr_status.fetch_repo_status_gh("rrnewton/hermit", net_wrapper=[])
        self.assertEqual(status.open, 0)  # legit empty list == 0 PRs (query OK)
        sleep.assert_called_once()

    @mock.patch("pr_status.hosted_authority_states", return_value={})
    @mock.patch("pr_status.banked_failure_tier_commits", return_value={})
    @mock.patch("pr_status.banked_green_commits", return_value=frozenset())
    @mock.patch("pr_status._cached_open_pr_census")
    @mock.patch("pr_status._fetch_open_prs_gh")
    def test_repo_deadline_is_shared_with_local_ledger(
        self,
        fetch: mock.Mock,
        census: mock.Mock,
        green: mock.Mock,
        _failure: mock.Mock,
        hosted: mock.Mock,
    ) -> None:
        """The ledger subprocess gets only the repo budget still remaining."""
        clock = [100.0]

        def fetch_ready(*_args: object, **_kwargs: object) -> list[object]:
            clock[0] += 2.0
            return []

        def fetch_census(*_args: object, **_kwargs: object) -> tuple[list, float]:
            clock[0] += 1.0
            return [], 0.0

        fetch.side_effect = fetch_ready
        census.side_effect = fetch_census
        with mock.patch("pr_status.time.monotonic", side_effect=lambda: clock[0]):
            pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[], timeout=10.0
            )

        self.assertAlmostEqual(green.call_args.kwargs["timeout"], 7.0)
        self.assertEqual(hosted.call_args.kwargs["deadline"], 110.0)

    # Isolate the exact-job setup-only path: the hosted authority dereference is
    # its own consumer with its own test class, and leaving it live here would
    # eat entries from the positional `side_effect` list below.
    @mock.patch("pr_status.hosted_authority_states", return_value={})
    @mock.patch("pr_status.banked_failure_tier_commits", return_value={})
    @mock.patch("pr_status.banked_green_commits", return_value=frozenset())
    @mock.patch("pr_status.subprocess.run")
    def test_fetch_consumer_dereferences_setup_failure(
        self,
        run: mock.Mock,
        _green: mock.Mock,
        _failure: mock.Mock,
        _hosted: mock.Mock,
    ) -> None:
        check = SetupOnlyRollupClassificationTests.failed_check()
        raw = [_pr(1665, [check], head=SetupOnlyRollupClassificationTests.HEAD)]
        fixture = (
            Path(__file__).resolve().parent / "fixtures/actions_job_92660569815.json"
        ).read_text()
        run.side_effect = [
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(raw), stderr=""
            ),
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=fixture, stderr=""
            ),
        ]
        with mock.patch(
            "pr_status._cached_open_pr_census", return_value=(raw, 0.0)
        ):
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[], timeout=30
            )
        self.assertEqual(status.pending, 1)
        self.assertEqual(status.red, 0)
        self.assertEqual(status.setup_only_no_result_checks, 1)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(
            run.call_args_list[1].args[0][-1],
            "repos/rrnewton/hermit/actions/jobs/92660569815",
        )

    @mock.patch("pr_status.hosted_authority_states", return_value={})
    @mock.patch("pr_status.banked_failure_tier_commits", return_value={})
    @mock.patch("pr_status.banked_green_commits", return_value=frozenset())
    @mock.patch("pr_status.subprocess.run")
    def test_fetch_consumer_preserves_genuine_product_red(
        self,
        run: mock.Mock,
        _green: mock.Mock,
        _failure: mock.Mock,
        _hosted: mock.Mock,
    ) -> None:
        check = SetupOnlyRollupClassificationTests.failed_check(
            name="P0 demo gate (demos 1-8)",
            run=31110129926,
            job=92645431859,
        )
        raw = [_pr(1697, [check], head="d93d512826c522dff89a27a1aa2d4eda0377796b")]
        fixture = (
            Path(__file__).resolve().parent / "fixtures/actions_job_92645431859.json"
        ).read_text()
        run.side_effect = [
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(raw), stderr=""
            ),
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=fixture, stderr=""
            ),
        ]
        with mock.patch(
            "pr_status._cached_open_pr_census", return_value=(raw, 0.0)
        ):
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[], timeout=30
            )
        self.assertEqual(status.pending, 0)
        self.assertEqual(status.red, 1)
        self.assertEqual(status.product_reds, 1)
        self.assertEqual(status.setup_only_no_result_checks, 0)

    def test_net_wrapper_default_prefers_with_proxy(self) -> None:
        with mock.patch("pr_status.shutil.which", return_value="/usr/bin/with-proxy"):
            self.assertEqual(pr_status.resolve_net_wrapper(None), ["with-proxy"])

    def test_net_wrapper_missing_named_is_error(self) -> None:
        with mock.patch("pr_status.shutil.which", return_value=None):
            with self.assertRaises(RuntimeError):
                pr_status.resolve_net_wrapper("nope-not-real")

    def test_net_wrapper_empty_disables(self) -> None:
        self.assertEqual(pr_status.resolve_net_wrapper(""), [])


class PlannerAdapterTests(unittest.TestCase):
    def test_command_uses_pinned_agent_utils_front_door(self) -> None:
        command = pr_status.planner_command("rrnewton/hermit", 7)
        self.assertEqual(command[1:3], ["pr-landing-planner", "status"])
        self.assertIn("--net-wrapper", command)
        self.assertIn("with-proxy", command)
        self.assertIn("--format", command)
        self.assertIn("json", command)

    def test_planning_runs_use_real_merge_tree_conflict_detection(self) -> None:
        # The merge-tree engine is ON for planning runs (the opt-in `--engine
        # planner` path). It was previously pinned to `file-overlap` on an
        # unmeasured "expensive fan-out" theory; the real cost was the fetch,
        # not the analysis (see planner_command). Guard the flip so it cannot
        # silently rot back to the cheap fallback.
        command = pr_status.planner_command("rrnewton/hermit", 7)
        idx = command.index("--conflict-detector")
        self.assertEqual(command[idx + 1], "merge-tree")
        self.assertNotIn("file-overlap", command)

    @mock.patch("pr_status.subprocess.run")
    def test_fetch_uses_planner_schema_without_reimplementing_ci(
        self, run: mock.Mock
    ) -> None:
        payload = {
            "summary": {
                "open": 3, "green": 1, "red": 1, "pending": 1,
                "real_reds": 1, "outage_suspected": False,
            },
            "prs": [{"pr": 12, "ci": "red", "red_class": "real", "title": "fix"}],
        }
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=0, stdout=json.dumps(payload), stderr=""
        )
        status = pr_status.fetch_repo_status_planner("rrnewton/hermit")
        self.assertEqual(status.open, 3)
        self.assertTrue(status.unhealthy)
        self.assertEqual(status.prs[0]["red_class"], "real")

    @mock.patch("pr_status.subprocess.run")
    def test_planner_bpfjailer_block_is_actionable(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="",
            stderr="blocked on this server based on a security policy",
        )
        with self.assertRaisesRegex(pr_status.RepoUnavailable, "BpfJailer"):
            pr_status.fetch_repo_status_planner("rrnewton/hermit")

    @mock.patch("pr_status.subprocess.run")
    def test_timeout_yields_unavailable_not_hang(self, run: mock.Mock) -> None:
        run.side_effect = subprocess.TimeoutExpired(cmd="planner", timeout=1.0)
        with self.assertRaises(pr_status.RepoUnavailable):
            pr_status.fetch_repo_status_planner("rrnewton/hermit", timeout=1.0)
        run.assert_called_once()  # a timeout is terminal, not retried


class OrchestrationTests(unittest.TestCase):
    @mock.patch("pr_status.subprocess.run")
    def test_collect_records_partial_result_on_timeout(self, run: mock.Mock) -> None:
        run.side_effect = subprocess.TimeoutExpired(cmd="gh", timeout=1.0)
        statuses = pr_status.collect_statuses(
            ["rrnewton/hermit"],
            warn_threshold=10,
            engine="gh",
            net_wrapper=[],
            per_repo_timeout=1.0,
            overall_deadline=5.0,
        )
        self.assertEqual(len(statuses), 1)
        self.assertFalse(statuses[0].available)
        self.assertFalse(statuses[0].unhealthy)

    @mock.patch("pr_status.time.sleep")
    @mock.patch("pr_status.subprocess.run")
    def test_full_query_504_still_surfaces_mechanism_overlap(
        self, run: mock.Mock, sleep: mock.Mock
    ) -> None:
        unavailable = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="", stderr="HTTP 504: Gateway Timeout"
        )
        labels_only = [
            _pr(1567, [], labels=("mechanism:cancel-in-progress",)),
            _pr(
                1575,
                [],
                draft=True,
                labels=("mechanism:cancel-in-progress",),
            ),
        ]
        run.side_effect = [
            unavailable,
            unavailable,
            unavailable,
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(labels_only), stderr=""
            ),
        ]
        statuses = pr_status.collect_statuses(
            ["rrnewton/hermit"],
            warn_threshold=10,
            engine="gh",
            net_wrapper=[],
            per_repo_timeout=30.0,
            overall_deadline=60.0,
        )
        self.assertFalse(statuses[0].available)
        self.assertEqual(len(statuses[0].mechanism_overlaps), 1)
        report = pr_status.render_report(statuses, warn_threshold=10, engine="gh")
        self.assertIn("UNAVAILABLE", report)
        self.assertIn("mechanism:cancel-in-progress", report)
        self.assertIn("#1567", report)
        self.assertIn("#1575", report)
        self.assertEqual(run.call_count, 4)
        self.assertEqual(sleep.call_count, 2)

    def test_collect_marks_unavailable_when_deadline_exhausted(self) -> None:
        statuses = pr_status.collect_statuses(
            ["rrnewton/hermit", "rrnewton/reverie"],
            warn_threshold=10,
            engine="gh",
            net_wrapper=[],
            per_repo_timeout=300.0,
            overall_deadline=-1.0,
        )
        self.assertEqual(len(statuses), 2)
        self.assertTrue(all(not status.available for status in statuses))
        self.assertIn("deadline", statuses[0].reason)

    def test_custom_gh_command_reaches_repo_query(self) -> None:
        status = pr_status._unavailable("rrnewton/hermit", "fixture")
        with mock.patch.object(
            pr_status, "fetch_repo_status_gh", return_value=status
        ) as fetch:
            statuses = pr_status.collect_statuses(
                ["rrnewton/hermit"],
                warn_threshold=10,
                engine="gh",
                net_wrapper=["custom-wrapper"],
                gh_cmd="custom-gh",
                per_repo_timeout=30.0,
                overall_deadline=60.0,
            )

        self.assertEqual(statuses, [status])
        fetch.assert_called_once()
        self.assertEqual(fetch.call_args.kwargs["net_wrapper"], ["custom-wrapper"])
        self.assertEqual(fetch.call_args.kwargs["gh_cmd"], "custom-gh")
        self.assertGreater(fetch.call_args.kwargs["timeout"], 0)

    def test_cli_accepts_custom_gh_command(self) -> None:
        args = pr_status.parse_args(["--gh-cmd", "/tmp/fake-gh"])
        self.assertEqual(args.gh_cmd, "/tmp/fake-gh")


class RenderTests(unittest.TestCase):
    def test_render_degraded_reports_partial(self) -> None:
        available = pr_status.RepoStatus(
            repo="rrnewton/reverie", open=1, green=1, red=0, pending=0,
            real_reds=0, outage_suspected=False, prs=(),
        )
        unavailable = pr_status._unavailable("rrnewton/hermit", "gh pr list failed")
        report = pr_status.render_report(
            [available, unavailable], warn_threshold=10, engine="gh"
        )
        self.assertIn("DEGRADED", report)
        self.assertIn("UNAVAILABLE", report)
        self.assertIn("PARTIAL RESULT", report)

    def test_render_distinguishes_benign_red_from_unhealthy(self) -> None:
        status = pr_status.RepoStatus(
            repo="rrnewton/hermit", open=2, green=1, red=1, pending=0,
            real_reds=0, outage_suspected=False, prs=(),
        )
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("CI health: HEALTHY", report)
        self.assertIn("red=1", report)
        self.assertIn("Verdict rule:", report)
        self.assertIn("local validation receipts", report)

    def test_unhealthy_verdict_names_exact_trigger_inputs(self) -> None:
        status = pr_status.RepoStatus(
            repo="rrnewton/hermit",
            open=4,
            green=1,
            red=3,
            pending=0,
            real_reds=2,
            outage_suspected=False,
            prs=(
                {"red_class": "real-red"},
                {"red_class": "real-red"},
                {"red_class": "stale-base"},
            ),
        )

        verdict = pr_status.health_verdict([status])
        self.assertEqual(verdict["state"], "unhealthy")
        self.assertEqual(verdict["inputs"][0]["real_reds"], 2)
        self.assertEqual(verdict["inputs"][0]["stale_base_reds"], 1)
        self.assertTrue(verdict["inputs"][0]["triggers_unhealthy"])
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("real_reds=2", report)
        self.assertIn("stale_base_reds=1", report)
        self.assertIn("triggers_unhealthy=True", report)

    def test_render_flags_gate_only_unhealthy_as_no_product_break(self) -> None:
        # All real-reds are gate reds => the note must say product-test reds=0.
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [
                _pr(
                    1,
                    [{"name": "merge-gate-v2", "status": "COMPLETED",
                      "conclusion": "FAILURE"}],
                    merge_state="BLOCKED",
                ),
            ],
        )
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("CI health: UNHEALTHY", report)
        self.assertIn("0 product-test reds", report)
        self.assertIn("(product=0 gate=1)", report)

    def test_render_points_at_product_red_when_present(self) -> None:
        status = pr_status._classify_gh_prs(
            "rrnewton/reverie",
            [
                _pr(
                    2,
                    [{"name": "Regular tests (GitHub-hosted)", "status": "COMPLETED",
                      "conclusion": "FAILURE"}],
                    merge_state="BLOCKED",
                ),
            ],
        )
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("CI health: UNHEALTHY", report)
        self.assertIn("1 product-test red(s)", report)
        self.assertIn("rrnewton/reverie=1", report)

    def test_render_surfaces_undetermined_caution(self) -> None:
        status = pr_status.RepoStatus(
            repo="rrnewton/hermit", open=3, green=0, red=3, pending=0,
            real_reds=0, undetermined_reds=3, outage_suspected=False, prs=(),
        )
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("undetermined_reds=3", report)
        self.assertIn("CAUTION", report)

    def test_render_separates_review_history_from_current_approval(self) -> None:
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            [
                _pr(
                    12,
                    [],
                labels=(
                    "post-facto-human-review",
                    "adversarial-review-codex2",
                    "adversarial-review-claude3",
                    "passed-review-claude",
                ),
                comments=(
                    "[adversarial-reviewer agent, CLAUDE]\n"
                    f"APPROVED-AT: claude {'a' * 40}",
                ),
            )
            ],
        )
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("dual_review=1", report)
        self.assertIn("current_dual_approval=0", report)
        self.assertIn("approval=partial", report)
        self.assertIn("canonical issue comments, inline review comments", report)
        self.assertIn("reporting only, not merge admission", report)
        self.assertIn("native reviews, and current GitHub actor permission", report)
        self.assertIn("the shared review authority", report)
        self.assertIn("review_permission_unavailable=0", report)
        self.assertIn("missing=current-approval-codex", report)


class ExecutedTestsCarveOutTests(unittest.TestCase):
    """The PR-facing peer of the ledger-side named-gate predicate: a GitHub
    product red whose EXACT head carries no named failing gate locally is a
    NO-RESULT, not a real red. Bind the demotion to the observed named-gate
    authority (gates[]), never a check or executed-test count (Proxy Binding)."""

    # The canonical same-check-count control pair (both rows report SIX gates;
    # only the named-gate evidence separates them). 40-hex heads padded from the
    # real ledger commit prefixes so the ledger row and the PR head bind by exact SHA.
    HEAD_GENUINE = "17b59fc6" + "1" * 32  # named failing gate -> stays RED (ok)
    HEAD_NO_RESULT = "98573d14" + "2" * 32  # no named gate, no count -> NO-RESULT
    HEAD_NEEDS_RERUN = "54b4d4e5" + "3" * 32  # failure count, no named gate -> NEEDS-RERUN

    def _product_fail(self, number: int, head: str):
        # A genuine PRODUCT-test failing check (not a gate meta-check), base known
        # and clean, so classification reaches the real-product-red branch where
        # the executed_tests carve-out applies.
        return _pr(
            number,
            [
                {
                    "name": "Regular tests (GitHub-hosted)",
                    "status": "COMPLETED",
                    "conclusion": "FAILURE",
                }
            ],
            merge_state="CLEAN",
            head=head,
        )

    def test_tier_derives_from_named_gate_not_check_count(self) -> None:
        # banked_failure_tier_commits reads the machine-local ledger and tiers each
        # fail row by the NAMED-GATE predicate. All control rows report six gates
        # (gates_run=6); only the named-gate evidence separates their tiers.
        import tempfile

        genuine_gate = [{"name": "portable CI DAG lane", "result": "fail",
                         "exit_code": 101, "real_seconds": 221}]
        ledger = "\n".join(
            json.dumps(row)
            for row in [
                {"result": "fail", "commit": self.HEAD_GENUINE,
                 "gates_run": 6, "gates_expected": 6, "failures": 1,
                 "gates": genuine_gate, "cwd": "/w/hermit"},
                {"result": "fail", "commit": self.HEAD_NO_RESULT,
                 "gates_run": 6, "gates_expected": 6, "gates": [],
                 "cwd": "/w/hermit"},
                {"result": "timeout", "commit": self.HEAD_NEEDS_RERUN,
                 "gates_run": 6, "gates_expected": 6, "failures": 1, "gates": [],
                 "cwd": "/w/hermit"},
                # strongest-wins: a second run at the genuine head with no named gate
                # must NOT downgrade the real named-gate failure.
                {"result": "fail", "commit": self.HEAD_GENUINE,
                 "gates_run": 6, "gates_expected": 6, "gates": [],
                 "cwd": "/w/hermit"},
                # repo isolation: a reverie fail must not appear for hermit.
                {"result": "fail", "commit": "b" * 40,
                 "gates": [], "cwd": "/w/reverie"},
                # a PASS row is not a failure tier.
                {"result": "pass", "commit": "c" * 40,
                 "gates": genuine_gate, "cwd": "/w/hermit"},
            ]
        )
        with tempfile.TemporaryDirectory() as d:
            rows = [json.loads(line) for line in ledger.splitlines()]
            with mock.patch.object(pr_status.canonical_ledger, "rows", return_value=rows):
                tiers = pr_status.banked_failure_tier_commits("rrnewton/hermit")
        self.assertEqual(tiers[self.HEAD_GENUINE], "ok")  # named gate wins over the no-gate dup
        self.assertEqual(tiers[self.HEAD_NO_RESULT], "no-result")
        self.assertEqual(tiers[self.HEAD_NEEDS_RERUN], "needs-rerun")
        self.assertNotIn("b" * 40, tiers)  # reverie isolated
        self.assertNotIn("c" * 40, tiers)  # pass is not a failure

    def test_no_result_red_demoted_but_genuine_red_stays(self) -> None:
        # PLANT BOTH WAYS in one call: the tests=1 head demotes to NO-RESULT while
        # the tests=760 full-failure head STILL classifies as a real product red.
        raw = [
            self._product_fail(1470, self.HEAD_GENUINE),
            self._product_fail(1443, self.HEAD_NO_RESULT),
            self._product_fail(9999, self.HEAD_NEEDS_RERUN),
        ]
        banked_failure = {
            self.HEAD_GENUINE: "ok",
            self.HEAD_NO_RESULT: "no-result",
            self.HEAD_NEEDS_RERUN: "needs-rerun",
        }
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit", raw, banked_failure=banked_failure
        )
        # Only the genuine full-suite failure is a real red; the other two demote.
        self.assertEqual(status.real_reds, 1)
        self.assertEqual(status.product_reds, 1)
        self.assertEqual(status.ledger_no_result, 1)
        self.assertEqual(status.ledger_needs_rerun, 1)
        by_pr = {pr["pr"]: pr for pr in status.prs}
        self.assertEqual(by_pr[1470]["red_class"], "real-red")
        self.assertEqual(by_pr[1470]["real_red_kind"], "product")
        self.assertEqual(by_pr[1443]["red_class"], "ledger-no-result")
        self.assertEqual(by_pr[9999]["red_class"], "ledger-needs-rerun")

    def test_no_local_row_keeps_github_verdict(self) -> None:
        # A red head with NO local ledger row is untouched — a hosted-only red keeps
        # the GitHub verdict (the ledger is machine-local; most heads have no row).
        raw = [self._product_fail(1200, "d" * 40)]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit", raw, banked_failure={}
        )
        self.assertEqual(status.real_reds, 1)
        self.assertEqual(status.ledger_no_result, 0)

    def test_gate_red_not_demoted_by_ledger_tier(self) -> None:
        # A landing-gate/review meta-check red is a genuine blocker regardless of the
        # local test count (a DIFFERENT authority) — it must NOT be demoted even if a
        # no-result ledger row exists at the head.
        raw = [
            _pr(
                1147,
                [{"name": "merge-gate-v4", "status": "COMPLETED",
                  "conclusion": "FAILURE"}],
                merge_state="BLOCKED",
                head=self.HEAD_NO_RESULT,
            )
        ]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            raw,
            banked_failure={self.HEAD_NO_RESULT: "no-result"},
        )
        self.assertEqual(status.real_reds, 1)
        self.assertEqual(status.gate_reds, 1)
        self.assertEqual(status.ledger_no_result, 0)

    def test_demotion_is_observable_in_render_and_verdict(self) -> None:
        # The demotion must not be silent: the health verdict and report both carry
        # the counts, so a red missing from real_reds is explained, not vanished.
        raw = [
            self._product_fail(1443, self.HEAD_NO_RESULT),
            self._product_fail(9999, self.HEAD_NEEDS_RERUN),
        ]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            raw,
            banked_failure={
                self.HEAD_NO_RESULT: "no-result",
                self.HEAD_NEEDS_RERUN: "needs-rerun",
            },
        )
        verdict = pr_status.health_verdict([status])
        self.assertEqual(verdict["state"], "healthy")  # both demoted -> not unhealthy
        self.assertEqual(verdict["inputs"][0]["ledger_no_result"], 1)
        self.assertEqual(verdict["inputs"][0]["ledger_needs_rerun"], 1)
        report = pr_status.render_report([status], warn_threshold=10, engine="gh")
        self.assertIn("ledger_no_result=1", report)
        self.assertIn("ledger_needs_rerun=1", report)

    def test_ledger_green_beats_failure_tier(self) -> None:
        # If the exact head has BOTH a green receipt and a fail row, the green
        # receipt (a complete PASS) wins: green_local, not a demoted no-result.
        raw = [self._product_fail(1624, self.HEAD_NO_RESULT)]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            raw,
            banked_green=frozenset({self.HEAD_NO_RESULT}),
            banked_failure={self.HEAD_NO_RESULT: "no-result"},
        )
        self.assertEqual(status.green_local, 1)
        self.assertEqual(status.ledger_no_result, 0)
        self.assertEqual(status.real_reds, 0)



HEAD_RED = "73652f90269c322ba0be298288ec5ff25f089993"    # hermit #1890, live
HEAD_GREEN = "c36f681eb05f8c9a832f84d89638b527a6283dcd"  # hermit #1897, live
HERMIT_AUTHORITY_JOB = "Regular tests (GitHub-managed portable)"


def _hosted_payload(sha: str, state: str, *, positive: int, required: int = 1):
    """A `ci-hub hosted-status --json` report, shaped like the live one."""
    job_state = state if state in ("green", "red") else "no_result"
    return {
        "authority": "github-actions-exact-head-jobs",
        "schema_version": 1,
        "policy_schema_version": 3,
        "repo": "rrnewton/hermit",
        "sha": sha,
        "state": state,
        "positive_count": positive,
        "required_positive_count": required,
        "jobs": [
            {
                "job_name": HERMIT_AUTHORITY_JOB,
                "workflow_name": "CI (GitHub-managed portable)",
                "workflow_file": ".github/workflows/ci-portable.yml",
                "state": job_state,
                "job_id": 93056597653,
                "run_id": 31238554232,
            }
        ],
        "last_poll_error": None,
    }


class HostedAuthorityReaderTests(unittest.TestCase):
    """`hosted_authority_for_head` is a CONSUMER of the one registered verifier.

    Bracketed both ways: a qualifying answer must FIRE (not be inert), and a
    well-shaped answer that does not bind to the requested authority/repo/sha
    must be REFUSED rather than believed.
    """

    def _run(self, returncode: int, stdout: str):
        return mock.patch(
            "pr_status.subprocess.run",
            return_value=subprocess.CompletedProcess(
                args=[], returncode=returncode, stdout=stdout, stderr=""
            ),
        )

    def _exists(self):
        return mock.patch("pr_status.Path.exists", return_value=True)

    def test_green_answer_fires(self) -> None:
        payload = json.dumps(_hosted_payload(HEAD_GREEN, "green", positive=1))
        with self._exists(), self._run(0, payload):
            authority = pr_status.hosted_authority_for_head(
                "rrnewton/hermit", HEAD_GREEN
            )
        self.assertEqual(authority.state, "green")
        self.assertEqual(authority.positive_count, 1)
        self.assertEqual(authority.required_positive_count, 1)
        self.assertEqual(authority.failing_job_names, ())

    def test_red_answer_fires_despite_nonzero_exit(self) -> None:
        # `hosted-status` exits 3 on red. The EXIT CODE IS NOT THE SIGNAL: a
        # reader that treated nonzero as "query failed" would demote every
        # genuine red to unknown, which is exactly the direction that lets a
        # broken head read as pending.
        payload = json.dumps(_hosted_payload(HEAD_RED, "red", positive=0))
        with self._exists(), self._run(3, payload):
            authority = pr_status.hosted_authority_for_head("rrnewton/hermit", HEAD_RED)
        self.assertEqual(authority.state, "red")
        self.assertEqual(authority.failing_job_names, (HERMIT_AUTHORITY_JOB,))

    def test_no_result_is_not_green_and_is_observed(self) -> None:
        payload = json.dumps(_hosted_payload(HEAD_RED, "no_result", positive=0))
        with self._exists(), self._run(4, payload):
            authority = pr_status.hosted_authority_for_head("rrnewton/hermit", HEAD_RED)
        self.assertEqual(authority.state, "no_result")
        self.assertTrue(authority.observed)

    def test_answer_for_another_sha_is_refused(self) -> None:
        # A perfectly well-formed GREEN report — for a DIFFERENT commit. Believing
        # it would let one head's authority authorize another's.
        payload = json.dumps(_hosted_payload(HEAD_GREEN, "green", positive=1))
        with self._exists(), self._run(0, payload):
            authority = pr_status.hosted_authority_for_head("rrnewton/hermit", HEAD_RED)
        self.assertEqual(authority.state, "unknown")
        self.assertFalse(authority.observed)
        self.assertIn("does not bind", authority.reason)

    def test_answer_for_another_repo_is_refused(self) -> None:
        payload = _hosted_payload(HEAD_GREEN, "green", positive=1)
        payload["repo"] = "rrnewton/reverie"
        with self._exists(), self._run(0, json.dumps(payload)):
            authority = pr_status.hosted_authority_for_head(
                "rrnewton/hermit", HEAD_GREEN
            )
        self.assertEqual(authority.state, "unknown")

    def test_answer_from_another_authority_is_refused(self) -> None:
        # The tampered case: a green report that never names the exact-head job
        # authority. Different authorities must not collapse into one another.
        payload = _hosted_payload(HEAD_GREEN, "green", positive=1)
        payload["authority"] = "ci-hub-local-receipt-policy-v1"
        with self._exists(), self._run(0, json.dumps(payload)):
            authority = pr_status.hosted_authority_for_head(
                "rrnewton/hermit", HEAD_GREEN
            )
        self.assertEqual(authority.state, "unknown")

    def test_unknown_state_vocabulary_is_refused(self) -> None:
        payload = _hosted_payload(HEAD_GREEN, "passed", positive=1)
        with self._exists(), self._run(0, json.dumps(payload)):
            authority = pr_status.hosted_authority_for_head(
                "rrnewton/hermit", HEAD_GREEN
            )
        self.assertEqual(authority.state, "unknown")

    def test_missing_binary_and_non_json_and_timeout_are_unknown(self) -> None:
        with mock.patch("pr_status.Path.exists", return_value=False):
            self.assertEqual(
                pr_status.hosted_authority_for_head(
                    "rrnewton/hermit", HEAD_GREEN
                ).state,
                "unknown",
            )
        with self._exists(), self._run(0, "not json at all"):
            self.assertEqual(
                pr_status.hosted_authority_for_head(
                    "rrnewton/hermit", HEAD_GREEN
                ).state,
                "unknown",
            )
        with self._exists(), mock.patch(
            "pr_status.subprocess.run",
            side_effect=subprocess.TimeoutExpired(cmd="ci-hub", timeout=1),
        ):
            self.assertEqual(
                pr_status.hosted_authority_for_head(
                    "rrnewton/hermit", HEAD_GREEN
                ).state,
                "unknown",
            )

    def test_parser_is_bound_to_the_real_verifier_output(self) -> None:
        """The shape we parse is the shape the verifier actually emits.

        Not a hand-written fixture: this drives the REAL
        `remediation/protocol.py hosted-status --json` code path with only its
        network call stubbed, then feeds its verbatim stdout to our reader. If
        the verifier's report schema moves, this fails instead of silently
        degrading every head to `unknown`.
        """
        import contextlib
        import io
        import types

        sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
        from remediation import protocol

        self.assertTrue(
            pr_status.HOSTED_STATUS_VERIFIER.exists(),
            f"verifier missing at {pr_status.HOSTED_STATUS_VERIFIER}",
        )
        run = {
            "databaseId": 31238567851,
            "headSha": HEAD_GREEN,
            "workflowFile": ".github/workflows/ci-portable.yml",
            "workflowName": "CI (GitHub-managed portable)",
            "status": "completed",
            "conclusion": "success",
            "event": "workflow_dispatch",
            "startedAt": "2026-08-08T04:02:55Z",
            "updatedAt": "2026-08-08T04:18:34Z",
            "url": "https://github.com/rrnewton/hermit/actions/runs/31238567851",
            "jobs": [
                {
                    "id": 93057001025,
                    "run_id": 31238567851,
                    "head_sha": HEAD_GREEN,
                    "name": HERMIT_AUTHORITY_JOB,
                    "status": "completed",
                    "conclusion": "success",
                    "started_at": "2026-08-08T04:02:55Z",
                    "completed_at": "2026-08-08T04:18:34Z",
                    "html_url": (
                        "https://github.com/rrnewton/hermit/actions/runs/"
                        "31238567851/job/93057001025"
                    ),
                }
            ],
        }
        buffer = io.StringIO()
        with mock.patch.object(protocol, "github_runs", return_value=[run]):
            with contextlib.redirect_stdout(buffer):
                exit_code = protocol.hosted_status(
                    types.SimpleNamespace(
                        repo="rrnewton/hermit", sha=HEAD_GREEN, json=True
                    )
                )
        emitted = buffer.getvalue().strip()
        self.assertEqual(exit_code, 0)
        with self._exists(), self._run(exit_code, emitted):
            authority = pr_status.hosted_authority_for_head(
                "rrnewton/hermit", HEAD_GREEN
            )
        self.assertEqual(authority.state, "green")
        self.assertEqual(authority.positive_count, 1)
        self.assertEqual(authority.required_positive_count, 1)

    def test_short_sha_is_never_queried(self) -> None:
        with mock.patch("pr_status.subprocess.run") as run:
            authority = pr_status.hosted_authority_for_head("rrnewton/hermit", "abc123")
        run.assert_not_called()
        self.assertEqual(authority.state, "unknown")


class HostedAuthorityClassificationTests(unittest.TestCase):
    """The rollup no longer decides CI state on its own.

    Regression guard for the measured defect: `statusCheckRollup` carried 0 of
    455 `workflow_dispatch`-produced check runs across hermit's 12 ready heads
    (2026-08-08), so under merge-gate-v4 the registered authority job is absent
    from it. Three heads whose authority job had FAILED were reported `pending`.
    """

    @staticmethod
    def _authority(sha: str, state: str, *, failing=()):
        return {
            sha: pr_status.HostedAuthority(
                state=state,
                positive_count=1 if state == "green" else 0,
                required_positive_count=1,
                failing_job_names=tuple(failing),
            )
        }

    def test_authority_red_under_an_empty_rollup_is_red_not_pending(self) -> None:
        # THE DEFECT. Rollup shows only the gate's own cancelled/skipped checks
        # (all NO_RESULT => pending); the authority job failed and is invisible.
        raw = [
            _pr(
                1890,
                _rollup(("COMPLETED", "CANCELLED"), ("COMPLETED", "SKIPPED")),
                head=HEAD_RED,
            )
        ]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            raw,
            hosted_authority=self._authority(
                HEAD_RED, "red", failing=(HERMIT_AUTHORITY_JOB,)
            ),
        )
        self.assertEqual(status.prs[0]["ci"], "red")
        self.assertEqual(status.prs[0]["rollup_ci"], "pending")
        self.assertEqual(status.prs[0]["hosted_authority"], "red")
        self.assertEqual(status.pending, 0)
        self.assertEqual(status.red, 1)
        self.assertEqual(status.real_reds, 1)
        # The authority job is a product test, so it must land in the product
        # bucket -- an empty rollup previously routed it to `gate`.
        self.assertEqual(status.product_reds, 1)
        self.assertEqual(status.gate_reds, 0)
        self.assertIn(HERMIT_AUTHORITY_JOB, status.prs[0]["failing_checks"])
        self.assertEqual(status.hosted_red, 1)

    def test_authority_green_under_a_pending_rollup_is_green(self) -> None:
        raw = [
            _pr(
                1897,
                _rollup(("COMPLETED", "CANCELLED"), ("COMPLETED", "SKIPPED")),
                head=HEAD_GREEN,
            )
        ]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            raw,
            hosted_authority=self._authority(HEAD_GREEN, "green"),
        )
        self.assertEqual(status.prs[0]["ci"], "green")
        self.assertEqual(status.prs[0]["rollup_ci"], "pending")
        self.assertEqual(status.green, 1)
        self.assertEqual(status.hosted_green, 1)
        self.assertEqual(status.prs[0]["hosted_positive"], "1/1")

    def test_rollup_red_dominates_a_green_authority(self) -> None:
        # A green product job does not clear a failing landing gate. Both are
        # reported; the head stays red, attributed to `gate`.
        raw = [
            _pr(
                1665,
                [
                    {
                        "name": "merge-gate-v4",
                        "status": "COMPLETED",
                        "conclusion": "FAILURE",
                    }
                ],
                head=HEAD_GREEN,
            )
        ]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            raw,
            hosted_authority=self._authority(HEAD_GREEN, "green"),
        )
        self.assertEqual(status.prs[0]["ci"], "red")
        self.assertEqual(status.prs[0]["hosted_authority"], "green")
        self.assertEqual(status.gate_reds, 1)
        self.assertEqual(status.product_reds, 0)

    def test_unobserved_authority_never_manufactures_green(self) -> None:
        # No entry in the map at all: we did not look. That must read as UNKNOWN
        # coverage, never as a positive, and the rollup alone still drives.
        raw = [_pr(1635, _rollup(("COMPLETED", "CANCELLED")), head=HEAD_RED)]
        status = pr_status._classify_gh_prs("rrnewton/hermit", raw)
        self.assertEqual(status.prs[0]["ci"], "pending")
        self.assertEqual(status.prs[0]["hosted_authority"], "unknown")
        self.assertEqual(status.prs[0]["hosted_positive"], "")
        self.assertEqual(status.green, 0)
        self.assertEqual(status.hosted_unknown, 1)
        self.assertEqual(status.hosted_green, 0)

    def test_authority_no_result_is_not_green(self) -> None:
        raw = [_pr(1635, [], head=HEAD_RED)]
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit",
            raw,
            hosted_authority=self._authority(HEAD_RED, "no_result"),
        )
        self.assertEqual(status.prs[0]["ci"], "pending")
        self.assertEqual(status.green, 0)
        self.assertEqual(status.hosted_green, 0)
        self.assertEqual(status.hosted_unknown, 0)

    def test_report_names_both_sources_per_head(self) -> None:
        raw = [
            _pr(1890, _rollup(("COMPLETED", "CANCELLED")), head=HEAD_RED),
            _pr(1897, _rollup(("COMPLETED", "CANCELLED")), head=HEAD_GREEN),
        ]
        authority = self._authority(HEAD_RED, "red", failing=(HERMIT_AUTHORITY_JOB,))
        authority.update(self._authority(HEAD_GREEN, "green"))
        status = pr_status._classify_gh_prs(
            "rrnewton/hermit", raw, hosted_authority=authority
        )
        report = pr_status.render_report([status], warn_threshold=99, engine="gh")
        self.assertIn("hosted authority (ci-hub hosted-status", report)
        self.assertIn("green=1 red=1 unobserved=0", report)
        self.assertIn("rollup=pending", report)
        self.assertIn("hosted=red", report)
        self.assertIn("hosted=green(1/1)", report)
        # The verdict must state that the rollup alone is not an input.
        self.assertIn("statusCheckRollup alone", report)


class HostedAuthorityFanOutTests(unittest.TestCase):
    def test_only_ready_heads_are_dereferenced(self) -> None:
        raw = [
            _pr(1, _rollup(("COMPLETED", "SUCCESS")), head="a" * 40),
            _pr(2, _rollup(("COMPLETED", "SUCCESS")), head="b" * 40, draft=True),
        ]
        with mock.patch(
            "pr_status.banked_green_commits", return_value=frozenset()
        ), mock.patch(
            "pr_status.banked_failure_tier_commits", return_value={}
        ), mock.patch(
            "pr_status.subprocess.run",
            return_value=subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(raw), stderr=""
            ),
        ), mock.patch(
            "pr_status.hosted_authority_states", return_value={}
        ) as states, mock.patch(
            "pr_status._cached_open_pr_census", return_value=(raw, 0.0)
        ):
            pr_status.fetch_repo_status_gh("rrnewton/hermit", net_wrapper=[])
        states.assert_called_once()
        self.assertEqual(states.call_args.args[1], ["a" * 40])

    def test_exhausted_deadline_leaves_heads_unobserved_not_green(self) -> None:
        with mock.patch("pr_status.hosted_authority_for_head") as reader:
            observed = pr_status.hosted_authority_states(
                "rrnewton/hermit",
                [HEAD_GREEN, HEAD_RED],
                deadline=time.monotonic() - 1.0,
            )
        reader.assert_not_called()
        self.assertEqual(observed, {})


class DraftCensusThrottleTests(unittest.TestCase):
    def test_census_cache_can_never_outlive_population_freshness(self) -> None:
        self.assertLessEqual(
            pr_status.DEFAULT_CENSUS_TTL_SECS,
            pr_status.CENSUS_FRESHNESS_BOUND_SECS,
        )

    @mock.patch("pr_status.subprocess.run")
    def test_expensive_graphql_query_is_ready_non_draft_only(self, run: mock.Mock) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="[]", stderr=""
        )
        pr_status._fetch_open_prs_gh(
            "rrnewton/hermit",
            fields=pr_status.GH_FIELDS,
            net_wrapper=[],
            search="draft:false",
        )
        command = run.call_args.args[0]
        self.assertIn("--search", command)
        self.assertEqual(command[command.index("--search") + 1], "draft:false")

    def test_lightweight_census_is_cached_within_freshness_bound(self) -> None:
        rows = [
            {"number": 1, "isDraft": False},
            {"number": 2, "isDraft": True},
        ]
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            pr_status, "_fetch_open_pr_census_rest", return_value=rows
        ) as fetch:
            cache = Path(directory)
            first, first_age = pr_status._cached_open_pr_census(
                "rrnewton/hermit", net_wrapper=[], cache_dir=cache, now=100.0
            )
            second, second_age = pr_status._cached_open_pr_census(
                "rrnewton/hermit", net_wrapper=[], cache_dir=cache, now=101.0
            )
        self.assertEqual(first, rows)
        self.assertEqual(second, rows)
        self.assertEqual(first_age, 0.0)
        self.assertEqual(second_age, 1.0)
        fetch.assert_called_once()

    def test_census_older_than_freshness_bound_refreshes_population(self) -> None:
        stale_rows = [
            {"number": number, "isDraft": True}
            for number in range(1, 39)
        ]
        current_rows = [
            {"number": number, "isDraft": True}
            for number in range(1, 41)
        ]
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            path = pr_status._census_cache_path("rrnewton/hermit", cache)
            pr_status._write_census_cache(path, stale_rows, now=100.0)
            with mock.patch.object(
                pr_status,
                "_fetch_open_pr_census_rest",
                return_value=current_rows,
            ) as fetch:
                rows, age = pr_status._cached_open_pr_census(
                    "rrnewton/hermit",
                    net_wrapper=[],
                    cache_dir=cache,
                    now=100.0 + 15 * 60 + 1.0,
                )
        self.assertEqual(len(rows), 40)
        self.assertEqual(age, 0.0)
        fetch.assert_called_once()

    def test_expired_census_is_never_returned_when_refresh_is_unavailable(self) -> None:
        stale_rows = [
            {"number": 1, "isDraft": False},
            {"number": 2, "isDraft": True},
        ]
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            path = pr_status._census_cache_path("rrnewton/hermit", cache)
            pr_status._write_census_cache(path, stale_rows, now=100.0)
            with mock.patch.object(
                pr_status,
                "_fetch_open_pr_census_rest",
                side_effect=pr_status.RepoUnavailable("REST refresh unavailable"),
            ) as fetch:
                with self.assertRaisesRegex(
                    pr_status.RepoUnavailable, "REST refresh unavailable"
                ):
                    pr_status._cached_open_pr_census(
                        "rrnewton/hermit",
                        net_wrapper=[],
                        cache_dir=cache,
                        now=100.0 + 15 * 60 + 1.0,
                    )
        fetch.assert_called_once()

    def test_ready_health_and_rare_census_populations_stay_separate(self) -> None:
        ready = [_pr(1, _rollup(("COMPLETED", "SUCCESS")), head="a" * 40)]
        census = [
            {"number": 1, "title": "ready", "isDraft": False, "labels": []},
            {"number": 2, "title": "draft", "isDraft": True, "labels": []},
            {"number": 3, "title": "draft", "isDraft": True, "labels": []},
        ]
        with mock.patch.object(
            pr_status, "_fetch_open_prs_gh", return_value=ready
        ) as graphql, mock.patch.object(
            pr_status, "_cached_open_pr_census", return_value=(census, 0.0)
        ), mock.patch.object(
            pr_status, "banked_green_commits", return_value=frozenset()
        ), mock.patch.object(
            pr_status, "banked_failure_tier_commits", return_value={}
        ), mock.patch.object(
            pr_status, "hosted_authority_states", return_value={}
        ):
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[]
            )
        self.assertEqual(graphql.call_args.kwargs["search"], "draft:false")
        self.assertEqual(status.open, 1)
        self.assertEqual(status.all_open, 3)
        self.assertEqual(status.drafts, 2)
        self.assertEqual(status.population_age_secs, 0.0)

    def test_ledger_unavailable_retains_population_but_refuses_health(self) -> None:
        ready = [_pr(1, _rollup(("COMPLETED", "SUCCESS")), head="a" * 40)]
        census = [
            {"number": 1, "title": "ready", "isDraft": False, "labels": []},
            {"number": 2, "title": "draft", "isDraft": True, "labels": []},
            {"number": 3, "title": "draft", "isDraft": True, "labels": []},
        ]
        with mock.patch.object(
            pr_status, "_fetch_open_prs_gh", return_value=ready
        ), mock.patch.object(
            pr_status, "_cached_open_pr_census", return_value=(census, 4.0)
        ), mock.patch.object(
            pr_status,
            "banked_green_commits",
            side_effect=pr_status.RepoUnavailable("ledger exploded"),
        ), mock.patch.object(
            pr_status, "banked_failure_tier_commits"
        ) as failure_rows, mock.patch.object(
            pr_status, "hosted_authority_states"
        ) as hosted:
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[]
            )

        self.assertFalse(status.available)
        self.assertEqual(status.open, 1)
        self.assertEqual(status.all_open, 3)
        self.assertEqual(status.drafts, 2)
        self.assertEqual(status.population_age_secs, 4.0)
        self.assertIn("ledger exploded", status.reason)
        self.assertEqual((status.green, status.red, status.pending), (0, 0, 0))
        failure_rows.assert_not_called()
        hosted.assert_not_called()

    def test_failure_ledger_unavailable_also_refuses_health(self) -> None:
        ready = [_pr(1, _rollup(("COMPLETED", "SUCCESS")), head="a" * 40)]
        census = [
            {"number": 1, "title": "ready", "isDraft": False, "labels": []}
        ]
        with mock.patch.object(
            pr_status, "_fetch_open_prs_gh", return_value=ready
        ), mock.patch.object(
            pr_status, "_cached_open_pr_census", return_value=(census, 1.0)
        ), mock.patch.object(
            pr_status, "banked_green_commits", return_value=frozenset()
        ), mock.patch.object(
            pr_status,
            "banked_failure_tier_commits",
            side_effect=pr_status.RepoUnavailable("failure ledger unreadable"),
        ), mock.patch.object(
            pr_status, "hosted_authority_states"
        ) as hosted:
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[]
            )

        self.assertFalse(status.available)
        self.assertEqual(status.all_open, 1)
        self.assertEqual(status.open, 1)
        self.assertIn("failure ledger unreadable", status.reason)
        hosted.assert_not_called()

    def test_failed_rare_census_does_not_discard_ready_health(self) -> None:
        ready = [_pr(1, _rollup(("COMPLETED", "SUCCESS")), head="a" * 40)]
        with mock.patch.object(
            pr_status, "_fetch_open_prs_gh", return_value=ready
        ), mock.patch.object(
            pr_status,
            "_cached_open_pr_census",
            side_effect=pr_status.RepoUnavailable("REST quota unavailable"),
        ), mock.patch.object(
            pr_status, "banked_green_commits", return_value=frozenset()
        ), mock.patch.object(
            pr_status, "banked_failure_tier_commits", return_value={}
        ), mock.patch.object(
            pr_status, "hosted_authority_states", return_value={}
        ):
            status = pr_status.fetch_repo_status_gh(
                "rrnewton/hermit", net_wrapper=[]
            )
        self.assertEqual(status.open, 1)
        self.assertIsNone(status.all_open)
        self.assertIsNone(status.drafts)
        self.assertIn("REST quota", status.population_error)


if __name__ == "__main__":
    unittest.main()
