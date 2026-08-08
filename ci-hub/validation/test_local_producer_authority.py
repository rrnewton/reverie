#!/usr/bin/env python3
"""End-to-end producer-map brackets for local ledger authority.

The fixtures are real commits descended from the parent-pinned Hermit history,
so validate-status/newest-green derive blobs from git rather than trusting a
row field.  Primary and candidate maps are whole-map positives; a crossed or
unregistered commit is a full clean row that must still be refused.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
HUB = ROOT / "ci-hub" / "ci-hub"
HERMIT = ROOT / "hermit"


def run(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        list(args),
        cwd=cwd,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


class LocalProducerAuthorityTests(unittest.TestCase):
    def setUp(self) -> None:
        if subprocess.run(
            ["git", "-C", str(HERMIT), "rev-parse", "--git-dir"],
            capture_output=True,
        ).returncode != 0:
            self.skipTest("Hermit submodule is not initialized")

    def fixture(self, root: Path) -> tuple[Path, Path, dict[str, str]]:
        repo = root / "hermit-fixture"
        run("git", "clone", "-q", "--shared", str(HERMIT), str(repo))
        primary = json.loads((ROOT / "ci-hub/validate/producer-definition.json").read_text())
        primary_sha = primary["registered_at"]
        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)

        git_env = {
            **os.environ,
            "GIT_AUTHOR_NAME": "producer-authority-test",
            "GIT_AUTHOR_EMAIL": "test@example.com",
            "GIT_COMMITTER_NAME": "producer-authority-test",
            "GIT_COMMITTER_EMAIL": "test@example.com",
        }

        (repo / "validate.sh").write_text("#!/usr/bin/env bash\necho candidate\n")
        (repo / "ci/validate_peer_snapshot.py").write_text(
            "#!/usr/bin/env python3\nprint('candidate helper')\n"
        )
        run(
            "git",
            "add",
            "validate.sh",
            "ci/validate_peer_snapshot.py",
            cwd=repo,
            env=git_env,
        )
        run("git", "commit", "-qm", "candidate producer", cwd=repo, env=git_env)
        candidate_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)
        (repo / "validate.sh").write_text("#!/usr/bin/env bash\necho candidate\n")
        run("git", "add", "validate.sh", cwd=repo, env=git_env)
        run("git", "commit", "-qm", "validate-only producer", cwd=repo, env=git_env)
        crossed_validate_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)
        (repo / "ci/validate_peer_snapshot.py").write_text(
            "#!/usr/bin/env python3\nprint('candidate helper')\n"
        )
        run("git", "add", "ci/validate_peer_snapshot.py", cwd=repo, env=git_env)
        run("git", "commit", "-qm", "helper-only producer", cwd=repo, env=git_env)
        crossed_helper_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)
        run("git", "commit", "--allow-empty", "-qm", "later legacy map", cwd=repo, env=git_env)
        legacy_replay_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        def blob(sha: str, path: str) -> str:
            return run("git", "rev-parse", f"{sha}:{path}", cwd=repo)

        primary_map = {
            path: blob(primary_sha, path)
            for path in ("validate.sh", ".github/workflows/ci-portable.yml")
        }
        candidate_map = {
            path: blob(candidate_sha, path)
            for path in (
                "validate.sh",
                ".github/workflows/ci-portable.yml",
                "ci/validate_peer_snapshot.py",
            )
        }
        registry = {
            "registered_at": primary_sha,
            "registered_coverage_status": "legacy-selected-paths",
            "registered_valid_commits": [primary_sha],
            "registered": primary_map,
            "transition": {
                "id": "rrnewton-hermit-pr-999",
                "registered_at": candidate_sha,
                "provenance": {
                    "repository": "rrnewton/hermit",
                    "pull_request": 999,
                    "head": candidate_sha,
                },
                "finalize_after": "2098-01-01T00:00:00Z",
                "expires_at": "2099-01-01T00:00:00Z",
                "candidate_coverage_status": "complete",
                "added_paths": ["ci/validate_peer_snapshot.py"],
                "candidate": candidate_map,
            },
        }
        registry_path = root / "producer-definition.json"
        registry_path.write_text(json.dumps(registry))
        return repo, registry_path, {
            "primary": primary_sha,
            "candidate": candidate_sha,
            "crossed_validate": crossed_validate_sha,
            "crossed_helper": crossed_helper_sha,
            "legacy_replay": legacy_replay_sha,
        }

    @staticmethod
    def row(repo: Path, sha: str, finished: str) -> dict[str, object]:
        return {
            "schema_version": 4,
            "started_at": "2026-08-08T12:00:00Z",
            "finished_at": finished,
            "host": "producer-authority-fixture",
            "slot": "fixture",
            "repo": "hermit",
            "cwd": str(repo),
            "profile": "full",
            "selection_mode": "full",
            "commit": sha,
            "tree": run("git", "rev-parse", f"{sha}^{{tree}}", cwd=repo),
            "commit_anchored": True,
            "tree_dirty": False,
            "result": "pass",
            "raw_result": "pass",
            "exit_code": 0,
            "executed_tests": 12,
            "filtered_tests": 0,
            "checks": 1,
            "gates_run": 1,
            "gates_expected": 1,
            "failures": 0,
            "real_seconds": 1.0,
            "log_file": "/tmp/producer-authority-fixture.log",
            "gates": [{"name": "full", "result": "pass", "exit_code": 0}],
        }

    def hub(self, registry: Path, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(HUB), *args],
            cwd=ROOT,
            env={
                **os.environ,
                "CI_HUB_TOOL_COST_ACTIVE": "1",
                "PRODUCER_DEFINITION_REGISTRY": str(registry),
            },
            capture_output=True,
            text=True,
            timeout=120,
        )

    def test_validate_status_whole_map_and_versioned_reporting(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, registry, shas = self.fixture(root)
            cases = [
                ("primary", 0, "legacy-selected-paths", 2),
                ("candidate", 0, "complete", 3),
                ("crossed_validate", 4, None, 0),
                ("crossed_helper", 4, None, 0),
                ("legacy_replay", 4, None, 0),
            ]
            for index, (name, expected_rc, status, path_count) in enumerate(cases):
                with self.subTest(name=name):
                    ledger = root / f"{name}.jsonl"
                    ledger.write_text(
                        json.dumps(
                            self.row(repo, shas[name], f"2026-08-08T12:0{index}:00Z")
                        )
                        + "\n"
                    )
                    result = self.hub(
                        registry,
                        "validate-status",
                        "--sha",
                        shas[name],
                        "--repo",
                        "rrnewton/hermit",
                        "--ledger",
                        str(ledger),
                        "--json",
                    )
                    self.assertEqual(result.returncode, expected_rc, result.stderr)
                    report = json.loads(result.stdout)
                    if expected_rc == 0:
                        evidence = report["newest_qualifying"]["producer_definition"]
                        self.assertEqual(evidence["coverage_status"], status)
                        self.assertEqual(len(evidence["paths"]), path_count)
                        self.assertEqual(evidence["paths"], sorted(evidence["definition"]))
                        if name == "primary":
                            self.assertEqual(evidence["valid_commits"], [shas["primary"]])
                        else:
                            self.assertNotIn("valid_commits", evidence)
                    else:
                        self.assertEqual(report["qualifying_count"], 0)

            primary_only = json.loads(registry.read_text())
            primary_only.pop("transition")
            primary_only_registry = root / "primary-only-registry.json"
            primary_only_registry.write_text(json.dumps(primary_only))
            unregistered_candidate_ledger = root / "unregistered-candidate.jsonl"
            unregistered_candidate_ledger.write_text(
                json.dumps(
                    self.row(repo, shas["candidate"], "2026-08-08T12:09:00Z")
                )
                + "\n"
            )
            refused = self.hub(
                primary_only_registry,
                "validate-status",
                "--sha",
                shas["candidate"],
                "--repo",
                "rrnewton/hermit",
                "--ledger",
                str(unregistered_candidate_ledger),
                "--json",
            )
            self.assertEqual(refused.returncode, 4, refused.stderr)
            self.assertEqual(json.loads(refused.stdout)["qualifying_count"], 0)

    def test_newest_green_rechecks_registry_and_expiry_key(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, registry, shas = self.fixture(root)
            candidate = shas["candidate"]
            run("git", "update-ref", "refs/remotes/origin/main", candidate, cwd=repo)
            ledger = root / "ledger.jsonl"
            ledger.write_text(
                json.dumps(self.row(repo, candidate, "2026-08-08T12:10:00Z")) + "\n"
            )
            cache = root / "newest-green-cache.json"
            args = (
                "newest-green",
                "--branch",
                "main",
                "--repo-dir",
                str(repo),
                "--ledger",
                str(ledger),
                "--cache",
                str(cache),
                "--no-fetch",
                "--json",
            )
            positive = self.hub(registry, *args)
            self.assertEqual(positive.returncode, 0, positive.stderr)
            report = json.loads(positive.stdout)["report"]
            evidence = report["green"]["producer_definition"]
            self.assertEqual(evidence["coverage_status"], "complete")
            self.assertEqual(len(evidence["paths"]), 3)

            tampered = json.loads(registry.read_text())
            tampered["transition"]["candidate"]["validate.sh"] = "0" * 40
            tampered_path = root / "tampered-registry.json"
            tampered_path.write_text(json.dumps(tampered))
            refused = self.hub(tampered_path, *args)
            self.assertEqual(refused.returncode, 4, refused.stderr)
            self.assertEqual(json.loads(refused.stdout)["verdict"], "NOT-VALIDATED")


if __name__ == "__main__":
    unittest.main()
