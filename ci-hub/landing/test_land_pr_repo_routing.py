#!/usr/bin/env python3
"""Regression bracket for repository routing in the canonical PR lander."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LANDER = ROOT / "ci-hub/landing/land-pr.sh"
AUTHORITY = ROOT / "ci-hub/landing/exact-head-validation-authority.sh"


class LandPrRepoRoutingTest(unittest.TestCase):
    def parse(self, *extra: str) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["CI_HUB_DOCS_PARSE_ONLY"] = "1"
        return subprocess.run(
            [str(LANDER), "470", "feature", *extra],
            cwd=ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
        )

    def test_default_repository_remains_hermit(self) -> None:
        result = self.parse("--no-rebase")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("repo=rrnewton/hermit", result.stdout)

    def test_explicit_reverie_repository_parses(self) -> None:
        result = self.parse("--repo", "rrnewton/reverie", "--no-rebase")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("repo=rrnewton/reverie", result.stdout)

    def test_unknown_repository_and_reverie_union_fail_closed(self) -> None:
        unknown = self.parse("--repo", "rrnewton/agent-utils")
        self.assertEqual(unknown.returncode, 2)
        self.assertIn("unsupported --repo", unknown.stderr)

        union = self.parse("--repo", "rrnewton/reverie", "--union")
        self.assertEqual(union.returncode, 2)
        self.assertIn("--union is defined only for rrnewton/hermit", union.stderr)

    def test_checkout_origin_must_match_selected_repository(self) -> None:
        rejected = [
            "https://github.com/rrnewton/hermit.git",
            "https://evilgithub.com/rrnewton/reverie.git",
            "https://github.com@evil.example/rrnewton/reverie.git",
            "https://github.com:443/rrnewton/reverie.git",
            "https://github.com/rrnewton/reverie.git/extra",
        ]
        for remote in rejected:
            with self.subTest(remote=remote):
                with tempfile.TemporaryDirectory() as directory:
                    checkout = Path(directory)
                    subprocess.run(
                        ["git", "init", "-q", str(checkout)],
                        check=True,
                        timeout=10,
                    )
                    subprocess.run(
                        [
                            "git",
                            "-C",
                            str(checkout),
                            "remote",
                            "add",
                            "origin",
                            remote,
                        ],
                        check=True,
                        timeout=10,
                    )
                    result = subprocess.run(
                        [
                            str(LANDER),
                            "470",
                            "feature",
                            "--repo",
                            "rrnewton/reverie",
                            "--checkout",
                            str(checkout),
                            "--foreground",
                        ],
                        cwd=ROOT,
                        text=True,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        timeout=10,
                    )
            self.assertEqual(result.returncode, 2)
            self.assertIn("does not match --repo rrnewton/reverie", result.stderr)

    def test_github_remote_normalizer_accepts_only_canonical_forms(self) -> None:
        script = LANDER.read_text(encoding="utf-8")
        start = script.index("normalize_github_remote(){")
        end = script.index("\n}\n", start) + 2
        probe = script[start:end] + '\nnormalize_github_remote "$1"'

        accepted = [
            "https://github.com/rrnewton/reverie",
            "https://github.com/rrnewton/reverie.git",
            "git@github.com:rrnewton/reverie",
            "git@github.com:rrnewton/reverie.git",
            "ssh://git@github.com/rrnewton/reverie",
            "ssh://git@github.com/rrnewton/reverie.git",
        ]
        for remote in accepted:
            with self.subTest(remote=remote):
                result = subprocess.run(
                    ["bash", "-c", probe, "probe", remote],
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), "rrnewton/reverie")

        rejected = [
            "http://github.com/rrnewton/reverie.git",
            "https://evilgithub.com/rrnewton/reverie.git",
            "https://github.com@evil.example/rrnewton/reverie.git",
            "https://github.com:443/rrnewton/reverie.git",
            "https://github.com/rrnewton/reverie.git/extra",
        ]
        for remote in rejected:
            with self.subTest(remote=remote):
                result = subprocess.run(
                    ["bash", "-c", probe, "probe", remote],
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=10,
                )
                self.assertNotEqual(result.returncode, 0)

    def test_repository_is_preserved_across_wrappers_and_authorities(self) -> None:
        script = LANDER.read_text(encoding="utf-8")
        self.assertIn(
            'detached_args=(--_detached "$PR" "$BR" --repo "$R"', script
        )
        self.assertIn('args=(--_inner "$PR" "$BR" --repo "$R"', script)
        # The exact-head status read moved into
        # `attempt_jit_exact_head_validation`, so its sha is that function's
        # `$candidate` parameter. The property under test here is that the
        # repository is still threaded as "$R" through every wrapper and
        # authority, and it is.
        self.assertIn('validate-status --repo "$R" --sha "$candidate"', script)
        self.assertIn('--repo "$R" --result "$candidate"', script)
        self.assertIn('apply-local-label --pr "$PR" --repo "$R"', script)
        self.assertIn('--repo "$R" --pr "$PR" --json', script)
        self.assertIn('"repos/$R/issues/$PR/comments?per_page=100"', script)
        merge_token = "gh pr" + ' merge "$PR" -R "$R" --rebase'
        self.assertIn(merge_token, script)
        self.assertIn('--match-head-commit "$HEAD"', script)
        self.assertIn('git -C "$WT" merge-base --is-ancestor', script)
        self.assertNotIn('git -C "$ROOT/hermit" fetch', script)

    def test_reverie_uses_local_receipt_without_hermit_comment_finalizer(self) -> None:
        script = LANDER.read_text(encoding="utf-8")
        self.assertIn('if [ "$R" = rrnewton/hermit ]; then', script)
        self.assertIn(
            'receipt_detail=$(validation_landability "$HEAD" "$current_base" 2>&1)',
            script,
        )
        authority = AUTHORITY.read_text(encoding="utf-8")
        self.assertIn(
            '$repo != rrnewton/hermit && $repo != rrnewton/reverie', authority
        )
        self.assertIn("required_coverage=local:full-six-gate-v2", authority)
        self.assertIn("required_coverage=local:full-seven-gate-v3", authority)

    def test_merge_remains_guarded_and_serialized(self) -> None:
        script = LANDER.read_text(encoding="utf-8")
        lock = script.index('"$ROOT/ci-hub/ci-hub" land-lock run')
        inner = script.index("# --- inner: we now hold the land-lock")
        merge_token = "gh pr" + ' merge "$PR"'
        merge = script.index(
            "out=$(with-proxy " + merge_token + ' -R "$R" --rebase'
        )
        self.assertLess(lock, inner)
        self.assertLess(inner, merge)
        self.assertEqual(script.count("out=$(with-proxy " + merge_token), 1)
        self.assertNotIn("--admin", script[merge : merge + 300])


if __name__ == "__main__":
    unittest.main()
