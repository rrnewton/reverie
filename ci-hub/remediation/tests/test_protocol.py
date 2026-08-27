from __future__ import annotations

import argparse
import io
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

SCRIPT_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPT_DIR))

import protocol

SHA = "a" * 40
NEXT_SHA = "b" * 40


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
                "completed_at": "2026-08-05T00:09:00Z" if status == "completed" else None,
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


class ProtocolTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def repo_source(self, repo: str) -> Path:
        source = self.root / repo.rsplit("/", 1)[1]
        subprocess.run(["git", "init", "-q", str(source)], check=True)
        subprocess.run(
            ["git", "-C", str(source), "remote", "add", "origin", f"https://github.com/{repo}.git"],
            check=True,
        )
        return source

    def test_fetch_target_uses_bounded_tracking_ref_helper(self) -> None:
        source = Path("/source")
        with mock.patch.object(protocol.fetch_ref, "fetch_remote_tracking_branch") as fetch:
            self.assertEqual(protocol._fetch_target(source, "main"), "origin/main")
        fetch.assert_called_once_with(
            source,
            remote="origin",
            branch="main",
            use_proxy=True,
            network_timeout_s=protocol.DEFAULT_NETWORK_TIMEOUT,
        )

    def test_repo_source_binds_exact_github_repository(self) -> None:
        source = self.repo_source("rrnewton/hermit")
        self.assertEqual(
            protocol.resolve_repo_source("rrnewton/hermit", source), source.resolve()
        )
        subprocess.run(
            ["git", "-C", str(source), "remote", "set-url", "origin", "https://evilgithub.com/rrnewton/hermit.git"],
            check=True,
        )
        with self.assertRaisesRegex(protocol.ProtocolError, "not required repository"):
            protocol.resolve_repo_source("rrnewton/hermit", source)

    def test_verify_landing_brackets_pr_and_direct_commit(self) -> None:
        source = self.repo_source("rrnewton/hermit")
        pr_args = argparse.Namespace(
            source=source, reference="1219", target="main", repo="rrnewton/hermit",
            json=True, item=None, claimed_oid=None,
        )
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(protocol, "_query_pr_landing", return_value=("MERGED", "c" * 40, SHA)),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=True),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(protocol.verify_landing(pr_args), 0)
        self.assertEqual(json.loads(output.getvalue())["resolved_sha"], SHA)

        sha_args = argparse.Namespace(
            source=source, reference=SHA, target="main", repo="rrnewton/hermit",
            json=True, item=None, claimed_oid=None,
        )
        with (
            mock.patch.object(protocol, "_fetch_target", return_value="origin/main"),
            mock.patch.object(protocol, "_resolve_raw_sha", return_value=SHA),
            mock.patch.object(protocol, "_is_target_ancestor", return_value=False),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(protocol.verify_landing(sha_args), 1)
        self.assertEqual(json.loads(output.getvalue())["state"], "not-landed")

    def test_hosted_policy_requires_a_nonempty_exact_job_set(self) -> None:
        for repo, count in (
            ("rrnewton/hermit", 1),
            ("rrnewton/reverie", 2),
            (protocol.PARENT_REPO, 4),
        ):
            with self.subTest(repo=repo):
                policy = protocol.verification_policy_for_repo(repo)
                self.assertEqual(policy["github"]["required_positive_count"], count)
                self.assertEqual(len(policy["github"]["required_jobs"]), count)
                evidence = protocol._github_patch(hosted_evidence(repo), SHA, policy)["github"]
                self.assertEqual(evidence["state"], "green")
                self.assertEqual(evidence["positive_count"], count)

    def test_hosted_status_brackets_green_red_and_no_result(self) -> None:
        def observe(runs: list[dict]) -> tuple[int, dict]:
            args = argparse.Namespace(repo="rrnewton/hermit", sha=SHA, json=True)
            with (
                mock.patch.object(protocol, "github_runs", return_value=runs),
                redirect_stdout(io.StringIO()) as output,
            ):
                rc = protocol.hosted_status(args)
            return rc, json.loads(output.getvalue())

        rc, report = observe(hosted_evidence("rrnewton/hermit"))
        self.assertEqual((rc, report["state"]), (0, "green"))
        rc, report = observe(hosted_evidence("rrnewton/hermit", job_states=("red",)))
        self.assertEqual((rc, report["state"]), (3, "red"))
        rc, report = observe([])
        self.assertEqual((rc, report["state"]), (4, "no_result"))

    def test_hosted_evidence_refuses_wrong_sha_duplicate_and_vacuous_sets(self) -> None:
        policy = protocol.verification_policy_for_repo("rrnewton/reverie")
        stale = hosted_evidence("rrnewton/reverie", sha=NEXT_SHA)
        with self.assertRaisesRegex(protocol.ProtocolError, "expected exact SHA"):
            protocol._github_patch(stale, SHA, policy)

        duplicate = hosted_evidence("rrnewton/reverie")
        duplicate[0]["jobs"].append(dict(duplicate[0]["jobs"][0]))
        with self.assertRaisesRegex(protocol.ProtocolError, "duplicate required"):
            protocol._github_patch(duplicate, SHA, policy)

        invalid = json.loads(json.dumps(policy))
        invalid["github"]["required_positive_count"] = 0
        with self.assertRaisesRegex(protocol.ProtocolError, "invalid verification"):
            protocol._github_patch([], SHA, invalid)

    def test_removed_commands_are_not_parseable(self) -> None:
        for command in (
            "arm", "watch", "status", "wake-sent", "inherit",
            "cancel-watcher", "resolve", "authorize-revert", "_local-run",
        ):
            with self.subTest(command=command), self.assertRaises(SystemExit):
                protocol.parse_args([command])


if __name__ == "__main__":
    unittest.main()
